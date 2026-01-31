//! DOGKBD Receiver
//!
//! Receives keystrokes over UDP and injects them into a target window.

mod app;
mod inject;
mod keys;
mod net;
mod target;

use clap::Parser;
use std::sync::mpsc;

/// DOGKBD receiver GUI for Windows/Linux
#[derive(Parser, Debug)]
#[command(name = "dogkbd-receiver")]
#[command(about = "Receives keystrokes over UDP and injects them")]
struct Args {
    /// UDP port to listen on
    #[arg(short, long, default_value_t = 44555)]
    port: u16,
}

fn main() -> eframe::Result<()> {
    let args = Args::parse();

    // Create channel for network -> GUI communication
    let (tx, rx) = mpsc::channel();

    // Start network listener thread
    let port = args.port;
    match net::start_listener(port, tx) {
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

    eframe::run_native(
        "DOGKBD Receiver",
        options,
        Box::new(|_cc| Ok(Box::new(app::DogkbdApp::new(rx)))),
    )
}
