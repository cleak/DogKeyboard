//! DOGKBD Pi-side exhibit display.
//!
//! Listens for router telemetry on UDP (newline-JSON, default port 44556),
//! de-duplicates the duplicate-sends, keeps a small snapshot so a refreshed kiosk
//! catches up instantly, and fans every event out to browser WebSocket clients.
//! All rendering decisions live in the browser (`display.html`) — the Pi owns the
//! display.

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::{Html, IntoResponse};
use axum::routing::get;
use axum::Router;
use clap::Parser;
use dogkbd_proto::telem::{TelemetryEvent, TelemetryKind, TELEM_PORT};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::net::UdpSocket;
use tokio::sync::broadcast;

const DISPLAY_HTML: &str = include_str!("../display.html");

#[derive(Parser, Debug)]
#[command(name = "dogkbd-display")]
#[command(about = "Receives DOGKBD telemetry and serves the exhibit monitor web app")]
struct Args {
    /// UDP port to receive telemetry on
    #[arg(long, default_value_t = TELEM_PORT)]
    telemetry_port: u16,

    /// HTTP port for the display web app
    #[arg(long, default_value_t = 8090)]
    web_port: u16,
}

/// Latest-known state, replayed to a browser the moment it connects so a kiosk
/// refresh shows a full screen immediately instead of waiting for live events.
#[derive(Default)]
struct Snapshot {
    hello: Option<String>,
    game: Option<String>,
    trophy: Option<String>,
    buffer: Option<String>,
    decision: Option<String>,
}

#[derive(Clone)]
struct AppState {
    tx: broadcast::Sender<String>,
    snapshot: Arc<Mutex<Snapshot>>,
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let args = Args::parse();

    let (tx, _rx) = broadcast::channel::<String>(1024);
    let snapshot = Arc::new(Mutex::new(Snapshot::default()));

    // UDP telemetry receiver task.
    let udp_tx = tx.clone();
    let udp_snapshot = snapshot.clone();
    let telemetry_port = args.telemetry_port;
    tokio::spawn(async move {
        if let Err(e) = run_telemetry_listener(telemetry_port, udp_tx, udp_snapshot).await {
            eprintln!("[display] telemetry listener error: {e}");
        }
    });

    // HTTP + WebSocket server.
    let state = AppState { tx, snapshot };
    let app = Router::new()
        .route("/", get(serve_display))
        .route("/ws", get(ws_handler))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{}", args.web_port)).await?;
    println!("DOGKBD display serving on http://0.0.0.0:{}/", args.web_port);
    println!("Receiving telemetry on UDP {}", args.telemetry_port);
    axum::serve(listener, app).await
}

/// Receive telemetry datagrams, de-dup the duplicate-sends, update the snapshot,
/// and rebroadcast each unique event line to browser clients.
async fn run_telemetry_listener(
    port: u16,
    tx: broadcast::Sender<String>,
    snapshot: Arc<Mutex<Snapshot>>,
) -> std::io::Result<()> {
    let socket = UdpSocket::bind(format!("0.0.0.0:{port}")).await?;
    let mut buf = vec![0u8; 4096];
    // Highest seq seen per router run, for de-dup + drop detection.
    let mut last_seq: HashMap<u32, u64> = HashMap::new();

    loop {
        let (len, _addr) = match socket.recv_from(&mut buf).await {
            Ok(v) => v,
            Err(e) => {
                eprintln!("[display] recv error: {e}");
                continue;
            }
        };
        let text = String::from_utf8_lossy(&buf[..len]);
        for line in text.split('\n') {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let ev = match TelemetryEvent::from_line(line) {
                Ok(e) => e,
                Err(_) => continue,
            };

            // De-dup: accept only strictly-newer seq per run.
            match last_seq.get(&ev.run_id) {
                Some(&prev) if ev.seq <= prev => continue,
                _ => {
                    last_seq.insert(ev.run_id, ev.seq);
                }
            }

            update_snapshot(&snapshot, &ev, line);
            // Ignore send errors (no subscribers yet).
            let _ = tx.send(line.to_string());
        }
    }
}

fn update_snapshot(snapshot: &Arc<Mutex<Snapshot>>, ev: &TelemetryEvent, line: &str) {
    let mut s = snapshot.lock().unwrap();
    match &ev.kind {
        TelemetryKind::Hello { .. } => s.hello = Some(line.to_string()),
        TelemetryKind::Game { .. } => s.game = Some(line.to_string()),
        TelemetryKind::Trophy { .. } => s.trophy = Some(line.to_string()),
        TelemetryKind::Buffer { .. } => s.buffer = Some(line.to_string()),
        TelemetryKind::RouteDecision { .. } => s.decision = Some(line.to_string()),
        _ => {}
    }
}

async fn serve_display() -> impl IntoResponse {
    Html(DISPLAY_HTML)
}

async fn ws_handler(ws: WebSocketUpgrade, State(state): State<AppState>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_ws(socket, state))
}

async fn handle_ws(mut socket: WebSocket, state: AppState) {
    // Replay the snapshot so a freshly-loaded kiosk is immediately populated.
    // Collect under the lock, then send after releasing it (guard isn't Send).
    let replay: Vec<String> = {
        let s = state.snapshot.lock().unwrap();
        [&s.hello, &s.game, &s.trophy, &s.buffer, &s.decision]
            .into_iter()
            .flatten()
            .cloned()
            .collect()
    };
    for line in replay {
        if socket.send(Message::Text(line.into())).await.is_err() {
            return;
        }
    }

    let mut rx = state.tx.subscribe();
    loop {
        match rx.recv().await {
            Ok(line) => {
                if socket.send(Message::Text(line.into())).await.is_err() {
                    break;
                }
            }
            Err(broadcast::error::RecvError::Lagged(n)) => {
                eprintln!("[display] WS client lagged, skipped {n}");
            }
            Err(broadcast::error::RecvError::Closed) => break,
        }
    }
}
