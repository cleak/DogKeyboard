//! Telemetry emitter — the reverse channel from the router to the Pi display.
//!
//! Owns a broadcast UDP socket and a per-run monotonic sequence. Any thread can
//! call [`Telemetry::emit`]; events are serialized to newline-JSON and
//! duplicate-sent (like `KeyTap`) so the display can de-dup on `(run_id, seq)`
//! and detect drops via `seq` gaps.

use dogkbd_proto::telem::{TelemetryEvent, TelemetryKind};
use std::net::UdpSocket;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone)]
pub struct Telemetry {
    inner: Arc<Inner>,
}

struct Inner {
    socket: Option<UdpSocket>,
    dest: String,
    run_id: u32,
    seq: AtomicU64,
    duplicate: u8,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn rand_run_id() -> u32 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    nanos ^ std::process::id()
}

impl Telemetry {
    /// Bind a broadcast UDP socket aimed at `addr:port`. On failure the emitter
    /// is created in a disabled state (emits become no-ops) so the router still
    /// runs without a display attached.
    pub fn new(addr: &str, port: u16, duplicate: u8) -> Self {
        let socket = UdpSocket::bind("0.0.0.0:0")
            .and_then(|s| {
                s.set_broadcast(true)?;
                Ok(s)
            })
            .map_err(|e| eprintln!("[telemetry] disabled (bind failed): {e}"))
            .ok();
        let run_id = rand_run_id();
        if socket.is_some() {
            println!("[telemetry] emitting to {addr}:{port} (run_id={run_id:#x}, x{duplicate})");
        }
        Self {
            inner: Arc::new(Inner {
                socket,
                dest: format!("{addr}:{port}"),
                run_id,
                seq: AtomicU64::new(0),
                duplicate: duplicate.max(1),
            }),
        }
    }

    /// A no-op emitter (no socket). Useful for tests and headless runs.
    #[allow(dead_code)] // used by tests and available for headless runs
    pub fn disabled() -> Self {
        Self {
            inner: Arc::new(Inner {
                socket: None,
                dest: String::new(),
                run_id: 0,
                seq: AtomicU64::new(0),
                duplicate: 1,
            }),
        }
    }

    #[allow(dead_code)] // part of the emitter's public surface
    pub fn run_id(&self) -> u32 {
        self.inner.run_id
    }

    /// Build, stamp, and duplicate-send a telemetry event.
    pub fn emit(&self, kind: TelemetryKind) {
        let Some(socket) = &self.inner.socket else {
            return;
        };
        let seq = self.inner.seq.fetch_add(1, Ordering::Relaxed);
        let ev = TelemetryEvent::new(self.inner.run_id, seq, now_ms(), kind);
        let line = ev.to_line();
        let bytes = line.as_bytes();
        for _ in 0..self.inner.duplicate {
            let _ = socket.send_to(bytes, &self.inner.dest);
        }
    }
}
