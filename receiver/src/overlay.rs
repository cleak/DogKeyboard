//! OBS overlay web server — serves HTML overlay and streams keystrokes via WebSocket.

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::{Html, IntoResponse};
use axum::routing::get;
use axum::Router;
use dogkbd_proto::{hid_to_us_ansi_char, KeyTap};
use serde::Serialize;
use std::sync::Arc;
use tokio::sync::broadcast;

const OVERLAY_HTML: &str = include_str!("../overlay.html");

/// A keystroke message sent to WebSocket clients as tagged JSON.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum KeystrokeMsg {
    #[serde(rename = "char")]
    Char {
        #[serde(rename = "char")]
        ch: String,
    },
    #[serde(rename = "enter")]
    Enter,
    #[serde(rename = "backspace")]
    Backspace,
    #[serde(rename = "space")]
    Space,
}

/// Convert a KeyTap into a KeystrokeMsg.
pub fn tap_to_msg(tap: &KeyTap) -> Option<KeystrokeMsg> {
    match tap.hid_code {
        0x28 => Some(KeystrokeMsg::Enter),
        0x2a => Some(KeystrokeMsg::Backspace),
        0x2c => Some(KeystrokeMsg::Space),
        _ => {
            let ch = hid_to_us_ansi_char(tap.hid_code, tap.shift())?;
            Some(KeystrokeMsg::Char {
                ch: ch.to_string(),
            })
        }
    }
}

#[derive(Clone)]
struct AppState {
    tx: Arc<broadcast::Sender<KeystrokeMsg>>,
}

async fn serve_overlay() -> impl IntoResponse {
    Html(OVERLAY_HTML)
}

async fn ws_handler(ws: WebSocketUpgrade, State(state): State<AppState>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_ws(socket, state))
}

async fn handle_ws(mut socket: WebSocket, state: AppState) {
    let mut rx = state.tx.subscribe();

    loop {
        match rx.recv().await {
            Ok(msg) => {
                let json = match serde_json::to_string(&msg) {
                    Ok(j) => j,
                    Err(_) => continue,
                };
                if socket.send(Message::Text(json.into())).await.is_err() {
                    break; // Client disconnected
                }
            }
            Err(broadcast::error::RecvError::Lagged(n)) => {
                eprintln!("WebSocket client lagged, skipped {n} messages");
            }
            Err(broadcast::error::RecvError::Closed) => break,
        }
    }
}

/// Run the HTTP/WebSocket server.
pub async fn run_web_server(
    web_port: u16,
    tx: broadcast::Sender<KeystrokeMsg>,
) -> std::io::Result<()> {
    let state = AppState {
        tx: Arc::new(tx),
    };

    let app = Router::new()
        .route("/", get(serve_overlay))
        .route("/ws", get(ws_handler))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{web_port}")).await?;
    println!("OBS overlay server listening on http://0.0.0.0:{web_port}/");

    axum::serve(listener, app).await?;
    Ok(())
}
