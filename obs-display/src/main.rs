mod net;
mod web;

use clap::Parser;
use tokio::sync::broadcast;

#[derive(Parser)]
#[command(name = "dogkbd-obs-display")]
#[command(about = "DOGKBD OBS Browser Source overlay")]
struct Args {
    /// UDP listen port
    #[arg(short, long, default_value_t = 44555)]
    port: u16,

    /// HTTP/WebSocket server port
    #[arg(short, long, default_value_t = 8080)]
    web_port: u16,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    let (tx, _rx) = broadcast::channel::<net::KeystrokeMsg>(256);

    let udp_tx = tx.clone();
    let udp_handle = tokio::spawn(async move {
        if let Err(e) = net::run_udp_listener(args.port, udp_tx).await {
            eprintln!("UDP listener error: {e}");
        }
    });

    let web_handle = tokio::spawn(async move {
        if let Err(e) = web::run_web_server(args.web_port, tx).await {
            eprintln!("Web server error: {e}");
        }
    });

    tokio::select! {
        _ = udp_handle => eprintln!("UDP listener exited"),
        _ = web_handle => eprintln!("Web server exited"),
    }

    Ok(())
}
