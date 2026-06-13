//! UDP network listener

use crate::overlay::{self, KeystrokeMsg};
use crate::telemetry::Telemetry;
use dogkbd_proto::telem::{Disposition, DropReason, TelemetryKind};
use dogkbd_proto::{hid_allowed, hid_to_decoded, KeyTap, PACKET_SIZE};
use std::collections::HashMap;
use std::net::UdpSocket;
use std::sync::mpsc::Sender;
use tokio::sync::broadcast;

/// Deduplication window size (number of sequence numbers to track per device)
const DEDUP_WINDOW: u32 = 100;

/// Emit a live keystroke telemetry event (panel 1 on the Pi).
fn emit_key(telem: &Telemetry, tap: &KeyTap, disposition: Disposition) {
    let decoded = hid_to_decoded(tap.hid_code, tap.shift()).unwrap_or_default();
    telem.emit(TelemetryKind::Keystroke {
        disposition,
        hid: tap.hid_code,
        shift: tap.shift(),
        decoded,
        device_id: tap.device_id,
    });
}

/// Start the UDP listener thread.
///
/// For every packet it emits live telemetry, then (for accepted, non-duplicate
/// keys) forwards the tap to the GUI (mpsc), the overlay WebSocket (broadcast),
/// and the router (mpsc, including Enter as a burst boundary).
pub fn start_listener(
    port: u16,
    tx: Sender<KeyTap>,
    overlay_tx: broadcast::Sender<KeystrokeMsg>,
    router_tx: Sender<KeyTap>,
    telem: Telemetry,
) -> std::io::Result<std::thread::JoinHandle<()>> {
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
                        emit_key(&telem, &tap, Disposition::Blocked);
                        telem.emit(TelemetryKind::Drop {
                            reason: DropReason::Blocked,
                            hid: Some(tap.hid_code),
                        });
                        continue;
                    }

                    // Deduplication: skip if we've seen this seq recently
                    if let Some(&last_seq) = seen.get(&tap.device_id) {
                        let diff = tap.seq.wrapping_sub(last_seq);
                        if diff == 0 || diff > (u32::MAX - DEDUP_WINDOW) {
                            // Same seq or wrapped-around duplicate
                            emit_key(&telem, &tap, Disposition::Dup);
                            continue;
                        }
                    }
                    seen.insert(tap.device_id, tap.seq);

                    // Live keystroke telemetry for every accepted key (incl. Enter).
                    emit_key(&telem, &tap, Disposition::Accepted);

                    // Forward to the router, including Enter as a burst boundary.
                    let _ = router_tx.send(tap);

                    // Filter out Enter key from the overlay/GUI path entirely.
                    if tap.hid_code == 0x28 {
                        continue;
                    }

                    // Send to overlay WebSocket clients
                    if let Some(msg) = overlay::tap_to_msg(&tap) {
                        let _ = overlay_tx.send(msg);
                    }

                    // Send to GUI
                    if tx.send(tap).is_err() {
                        // Channel closed, exit thread
                        break;
                    }
                }
                Err(e) => {
                    eprintln!("Socket error: {}", e);
                    break;
                }
            }
        }
    }))
}
