//! DOGKBD Receiver
//!
//! Receives keystrokes over UDP and injects them into a target window.

mod app;
mod inject;
mod keys;
mod net;
mod overlay;
mod target;

use clap::Parser;
use std::sync::atomic::AtomicBool;
use std::sync::{mpsc, Arc};
use tokio::sync::broadcast;

/// DOGKBD receiver GUI for Windows/Linux
#[derive(Parser, Debug)]
#[command(name = "dogkbd-receiver")]
#[command(about = "Receives keystrokes over UDP and injects them")]
struct Args {
    /// UDP port to listen on
    #[arg(short, long, default_value_t = 44555)]
    port: u16,

    /// HTTP port for OBS overlay web server
    #[arg(long, default_value_t = 8080)]
    web_port: u16,
}

fn main() -> eframe::Result<()> {
    let args = Args::parse();

    // Create channels
    let (tx, rx) = mpsc::channel();
    let (overlay_tx, _overlay_rx) = broadcast::channel::<overlay::KeystrokeMsg>(256);

    // Shared Claude Code busy flag (default: idle)
    let claude_busy = Arc::new(AtomicBool::new(false));

    // Start OBS overlay web server in a background thread with its own tokio runtime
    let web_port = args.web_port;
    let overlay_tx_clone = overlay_tx.clone();
    let claude_busy_web = claude_busy.clone();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().expect("Failed to create tokio runtime");
        rt.block_on(async {
            if let Err(e) = overlay::run_web_server(web_port, overlay_tx_clone, claude_busy_web).await {
                eprintln!("Web server error: {}", e);
            }
        });
    });

    // Start network listener thread
    let port = args.port;
    match net::start_listener(port, tx, overlay_tx) {
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
