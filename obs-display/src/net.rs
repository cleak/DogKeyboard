//! Async UDP listener with deduplication and allowlist filtering.

use dogkbd_proto::{hid_allowed, hid_to_us_ansi_char, KeyTap, PACKET_SIZE};
use serde::Serialize;
use std::collections::HashMap;
use tokio::net::UdpSocket;
use tokio::sync::broadcast;

/// Deduplication window size (matches receiver)
const DEDUP_WINDOW: u32 = 100;

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
fn tap_to_msg(tap: &KeyTap) -> Option<KeystrokeMsg> {
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

/// Run the async UDP listener. Decoded keystrokes are sent to `tx`.
pub async fn run_udp_listener(
    port: u16,
    tx: broadcast::Sender<KeystrokeMsg>,
) -> std::io::Result<()> {
    let socket = UdpSocket::bind(format!("0.0.0.0:{port}")).await?;
    println!("UDP listener bound on 0.0.0.0:{port}");

    let mut buf = [0u8; PACKET_SIZE];
    let mut seen: HashMap<u32, u32> = HashMap::new();

    loop {
        let (len, _addr) = socket.recv_from(&mut buf).await?;

        if len != PACKET_SIZE {
            continue;
        }

        let tap = match KeyTap::decode(&buf) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("Decode error: {e}");
                continue;
            }
        };

        // Receiver-side allowlist check
        if !hid_allowed(tap.hid_code) {
            eprintln!("Blocked HID code: {:#x}", tap.hid_code);
            continue;
        }

        // Deduplication: skip if we've seen this seq recently
        if let Some(&last_seq) = seen.get(&tap.device_id) {
            let diff = tap.seq.wrapping_sub(last_seq);
            if diff == 0 || diff > (u32::MAX - DEDUP_WINDOW) {
                continue;
            }
        }
        seen.insert(tap.device_id, tap.seq);

        // Convert to display message
        if let Some(msg) = tap_to_msg(&tap) {
            // Ignore send errors (no subscribers yet)
            let _ = tx.send(msg);
        }
    }
}
