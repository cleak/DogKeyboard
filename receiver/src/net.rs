//! UDP network listener

use dogkbd_proto::{hid_allowed, KeyTap, PACKET_SIZE};
use std::collections::HashMap;
use std::net::UdpSocket;
use std::sync::mpsc::Sender;

/// Deduplication window size (number of sequence numbers to track per device)
const DEDUP_WINDOW: u32 = 100;

/// Start the UDP listener thread
pub fn start_listener(port: u16, tx: Sender<KeyTap>) -> std::io::Result<std::thread::JoinHandle<()>> {
    let socket = UdpSocket::bind(format!("0.0.0.0:{}", port))?;
    socket.set_nonblocking(false)?;

    Ok(std::thread::spawn(move || {
        let mut buf = [0u8; PACKET_SIZE];
        // Track last seen sequence per device_id for deduplication
        let mut seen: HashMap<u32, u32> = HashMap::new();

        loop {
            match socket.recv_from(&mut buf) {
                Ok((len, _addr)) => {
                    if len != PACKET_SIZE {
                        continue;
                    }

                    let tap = match KeyTap::decode(&buf) {
                        Ok(t) => t,
                        Err(e) => {
                            eprintln!("Decode error: {}", e);
                            continue;
                        }
                    };

                    // Receiver-side allowlist check (safety belt)
                    if !hid_allowed(tap.hid_code) {
                        eprintln!("Blocked HID code: {:#x}", tap.hid_code);
                        continue;
                    }

                    // Deduplication: skip if we've seen this seq recently
                    if let Some(&last_seq) = seen.get(&tap.device_id) {
                        let diff = tap.seq.wrapping_sub(last_seq);
                        if diff == 0 || diff > (u32::MAX - DEDUP_WINDOW) {
                            // Same seq or wrapped-around duplicate
                            continue;
                        }
                    }
                    seen.insert(tap.device_id, tap.seq);

                    // Send to main thread
                    if tx.send(tap).is_err() {
                        // Channel closed, exit thread
                        break;
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(e) => {
                    eprintln!("Socket error: {}", e);
                    break;
                }
            }
        }
    }))
}
