//! DOGKBD Receiver
//!
//! Receives keystrokes over UDP and injects them into a target window.

mod app;
mod inject;
mod keys;
mod net;
mod overlay;
mod router;
mod target;
mod telemetry;

use clap::Parser;
use dogkbd_proto::telem::{TelemetryKind, TELEM_PORT};
use router::RouteTable;
use std::sync::atomic::AtomicBool;
use std::sync::{mpsc, Arc, Mutex};
use telemetry::Telemetry;
use tokio::sync::broadcast;

/// DOGKBD receiver GUI for Windows/Linux
#[derive(Parser, Debug)]
#[command(name = "dogkbd-receiver")]
#[command(about = "Receives keystrokes over UDP, routes them to Claude Code instances, and streams telemetry to the Pi display")]
struct Args {
    /// UDP port to listen on
    #[arg(short, long, default_value_t = 44555)]
    port: u16,

    /// HTTP port for OBS overlay + control web server
    #[arg(long, default_value_t = 8080)]
    web_port: u16,

    /// Destination address for telemetry to the Pi display (broadcast by default)
    #[arg(long, default_value = "255.255.255.255")]
    telemetry_addr: String,

    /// UDP port for telemetry to the Pi display
    #[arg(long, default_value_t = TELEM_PORT)]
    telemetry_port: u16,

    /// Duplicate-send count for telemetry datagrams (UDP reliability)
    #[arg(long, default_value_t = 2)]
    telemetry_duplicate: u8,
}

fn main() -> eframe::Result<()> {
    let args = Args::parse();

    // Create channels
    let (tx, rx) = mpsc::channel();
    let (router_tx, router_rx) = mpsc::channel();
    let (overlay_tx, _overlay_rx) = broadcast::channel::<overlay::KeystrokeMsg>(256);

    // Shared Claude Code busy flag (default: idle), the multiplexer route table,
    // and the telemetry emitter to the Pi display.
    let claude_busy = Arc::new(AtomicBool::new(false));
    let routes = Arc::new(Mutex::new(RouteTable::default_exhibit()));
    let telem = Telemetry::new(&args.telemetry_addr, args.telemetry_port, args.telemetry_duplicate);

    // Start OBS overlay + control web server in a background thread with its own tokio runtime
    let web_port = args.web_port;
    let overlay_tx_clone = overlay_tx.clone();
    let claude_busy_web = claude_busy.clone();
    let routes_web = routes.clone();
    let telem_web = telem.clone();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().expect("Failed to create tokio runtime");
        rt.block_on(async {
            if let Err(e) =
                overlay::run_web_server(web_port, overlay_tx_clone, claude_busy_web, routes_web, telem_web)
                    .await
            {
                eprintln!("Web server error: {}", e);
            }
        });
    });

    // Start the router (multiplexer) thread: buffers bursts and injects them
    // into the chosen Claude Code window.
    let routes_router = routes.clone();
    let telem_router = telem.clone();
    std::thread::spawn(move || {
        router::run_router(router_rx, routes_router, telem_router);
    });

    // Heartbeat: announce the route table to the display once per second.
    let routes_hb = routes.clone();
    let telem_hb = telem.clone();
    std::thread::spawn(move || loop {
        let (route_list, active) = {
            let t = routes_hb.lock().unwrap();
            (t.info_list(), t.override_active.clone())
        };
        telem_hb.emit(TelemetryKind::Hello { routes: route_list, active });
        std::thread::sleep(std::time::Duration::from_secs(1));
    });

    // Start network listener thread
    let port = args.port;
    match net::start_listener(port, tx, overlay_tx, router_tx, telem.clone()) {
        Ok(_handle) => {
            println!("Listening on UDP port {}", port);
        }
        Err(e) => {
            eprintln!("Failed to start listener: {}", e);
            return Ok(());
        }
    }

    // Run GUI
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([450.0, 650.0])
            .with_min_inner_size([350.0, 500.0]),
        ..Default::default()
    };

    let _result = eframe::run_native(
        "DOGKBD Receiver",
        options,
        Box::new(|_cc| Ok(Box::new(app::DogkbdApp::new(rx, claude_busy)))),
    );

    // Force-exit so the background tokio runtime / web server thread doesn't keep the process alive
    std::process::exit(0);
}
