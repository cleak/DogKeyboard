//! The multiplexer: a route table, a routing policy, and a burst-dispatch loop.
//!
//! Live keystrokes are streamed to the Pi from [`crate::net`] the instant they
//! arrive. This module governs only where *bursts* of input get injected: it
//! accumulates Momo's keys, and at each burst boundary (idle or Enter) picks an
//! eligible Claude Code destination and injects the burst into its window.
//!
//! Each destination is just a window we foreground + inject into; "is it ready?"
//! is the per-instance `busy`/`ready` state fed in over HTTP (see [`crate::overlay`]).

use crate::telemetry::Telemetry;
use dogkbd_proto::telem::{cap_text, DropReason, RouteInfo, TelemetryKind};
use dogkbd_proto::{hid_to_us_ansi_char, KeyTap};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Idle gap that ends a burst.
const BURST_IDLE: Duration = Duration::from_millis(2500);
/// Hard cap on buffered chars before we force a dispatch.
const BURST_MAX: usize = 280;
/// How long to let a foregrounded window settle before injecting.
const FOCUS_SETTLE: Duration = Duration::from_millis(150);

/// One routable Claude Code destination.
#[derive(Debug, Clone)]
pub struct Route {
    pub id: String,
    pub label: String,
    /// Case-insensitive substring matched against window title or process name.
    pub window_match: String,
    /// Higher wins when multiple routes are eligible.
    pub priority: i32,
    /// Only eligible when `ready` is true (the game waits for an input-accepting state).
    pub gated_on_ready: bool,
    pub busy: bool,
    pub ready: bool,
}

impl Route {
    fn info(&self) -> RouteInfo {
        RouteInfo {
            id: self.id.clone(),
            label: self.label.clone(),
            busy: self.busy,
            ready: self.ready,
        }
    }

    fn eligible(&self) -> bool {
        !self.busy && (!self.gated_on_ready || self.ready)
    }
}

/// Shared, mutable routing state. Read/written by HTTP handlers, the router
/// thread, and the GUI.
pub struct RouteTable {
    pub routes: Vec<Route>,
    /// Pinned route id, or `None` for automatic selection.
    pub override_active: Option<String>,
    /// Master switch; when false the router buffers/telemeters but never injects.
    pub enabled: bool,
}

impl RouteTable {
    /// Default exhibit table: prefer the game when it is ready, else the trophy factory.
    pub fn default_exhibit() -> Self {
        Self {
            routes: vec![
                Route {
                    id: "game".into(),
                    label: "Tea Leaves (game)".into(),
                    window_match: "tea-leaves".into(),
                    priority: 20,
                    gated_on_ready: true,
                    busy: false,
                    ready: false,
                },
                Route {
                    id: "trophy".into(),
                    label: "Trophy Factory".into(),
                    window_match: "trophy_factory".into(),
                    priority: 10,
                    gated_on_ready: false,
                    busy: false,
                    ready: true,
                },
            ],
            override_active: None,
            enabled: true,
        }
    }

    pub fn info_list(&self) -> Vec<RouteInfo> {
        self.routes.iter().map(Route::info).collect()
    }

    fn get(&self, id: &str) -> Option<&Route> {
        self.routes.iter().find(|r| r.id == id)
    }

    /// Update a route's status. Returns true if the route exists.
    pub fn set_status(&mut self, id: &str, busy: Option<bool>, ready: Option<bool>) -> bool {
        if let Some(r) = self.routes.iter_mut().find(|r| r.id == id) {
            if let Some(b) = busy {
                r.busy = b;
            }
            if let Some(rd) = ready {
                r.ready = rd;
            }
            true
        } else {
            false
        }
    }

    /// Apply the routing policy. Returns the chosen route id (or `None` to hold)
    /// and a human-readable reason for the telemetry.
    pub fn choose(&self) -> (Option<String>, String) {
        if !self.enabled {
            return (None, "routing disabled".into());
        }

        // Pinned override.
        if let Some(pin) = &self.override_active {
            return match self.get(pin) {
                Some(r) if r.eligible() => (Some(r.id.clone()), format!("pinned to {pin}")),
                Some(_) => (None, format!("waiting: pinned {pin} is busy/not-ready")),
                None => (None, format!("pinned route {pin} unknown")),
            };
        }

        // Automatic: highest-priority eligible route.
        let best = self
            .routes
            .iter()
            .filter(|r| r.eligible())
            .max_by_key(|r| r.priority);

        match best {
            Some(r) => (Some(r.id.clone()), format!("auto: {} eligible", r.id)),
            None => (None, "waiting: no eligible route".into()),
        }
    }

    fn window_match_for(&self, id: &str) -> Option<String> {
        self.get(id).map(|r| r.window_match.clone())
    }
}

/// Foreground the first window whose title or process matches; returns false if none.
#[cfg(windows)]
fn focus_window(window_match: &str) -> bool {
    let needle = window_match.to_lowercase();
    for w in crate::target::enumerate_windows() {
        if w.title.to_lowercase().contains(&needle)
            || w.process_name.to_lowercase().contains(&needle)
        {
            crate::target::set_foreground(w.hwnd);
            return true;
        }
    }
    false
}

#[cfg(not(windows))]
fn focus_window(_window_match: &str) -> bool {
    // No window targeting off Windows; injection (if any) goes to the focused app.
    true
}

/// Decoded display form of the buffered taps (Enter excluded — it ends the burst).
fn decode_buffer(taps: &[KeyTap]) -> String {
    let mut s = String::new();
    for t in taps {
        match t.hid_code {
            0x2c => s.push(' '),
            0x28 => {}
            _ => {
                if let Some(c) = hid_to_us_ansi_char(t.hid_code, t.shift()) {
                    s.push(c);
                }
            }
        }
    }
    s
}

/// Inject a finalized burst into the chosen route's window.
fn dispatch(table: &Arc<Mutex<RouteTable>>, telem: &Telemetry, route_id: &str, taps: &[KeyTap]) {
    let decoded = decode_buffer(taps);
    let window_match = {
        let t = table.lock().unwrap();
        t.window_match_for(route_id)
    };
    let Some(window_match) = window_match else {
        return;
    };

    if !focus_window(&window_match) {
        eprintln!("[router] window not found for route '{route_id}' (match '{window_match}')");
        telem.emit(TelemetryKind::Drop {
            reason: DropReason::NoRoute,
            hid: None,
        });
        return;
    }

    std::thread::sleep(FOCUS_SETTLE);

    let mut injected = 0usize;
    for t in taps {
        if t.hid_code == 0x28 {
            continue; // Enter handled below as auto-enter
        }
        match crate::inject::inject(t) {
            Ok(()) => injected += 1,
            Err(e) => eprintln!("[router] inject error: {e}"),
        }
    }
    // Auto-enter to submit the burst to the Claude Code prompt.
    let enter = KeyTap::new(0, 0, 0, 0x28);
    let auto_enter = crate::inject::inject(&enter).is_ok();

    println!("[router] dispatched {injected} chars to '{route_id}': {decoded:?}");
    telem.emit(TelemetryKind::Dispatch {
        route: route_id.to_string(),
        text: cap_text(&decoded),
        chars: injected,
        auto_enter,
    });
}

/// Run the routing loop. Consumes taps forwarded from [`crate::net`], buffers
/// them into bursts, and dispatches each burst to the chosen route.
pub fn run_router(rx: Receiver<KeyTap>, table: Arc<Mutex<RouteTable>>, telem: Telemetry) {
    let mut buffer: Vec<KeyTap> = Vec::new();
    let mut last_input: Option<Instant> = None;
    let mut announced_wait = false;

    loop {
        let timeout = Duration::from_millis(200);
        match rx.recv_timeout(timeout) {
            Ok(tap) => {
                let is_enter = tap.hid_code == 0x28;
                if !is_enter {
                    buffer.push(tap);
                    last_input = Some(Instant::now());
                    announced_wait = false;
                    let decoded = decode_buffer(&buffer);
                    telem.emit(TelemetryKind::Buffer {
                        route: None,
                        text: cap_text(&decoded),
                        len: decoded.chars().count(),
                    });
                }

                let boundary = is_enter || buffer.len() >= BURST_MAX;
                if boundary && !buffer.is_empty() {
                    try_dispatch(&table, &telem, &mut buffer, &mut announced_wait);
                    if buffer.is_empty() {
                        last_input = None;
                    }
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                if !buffer.is_empty() {
                    let idle = last_input.map(|t| t.elapsed()).unwrap_or(BURST_IDLE);
                    if idle >= BURST_IDLE {
                        try_dispatch(&table, &telem, &mut buffer, &mut announced_wait);
                        if buffer.is_empty() {
                            last_input = None;
                        }
                    }
                }
            }
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
}

/// Choose a route for the current buffer and either dispatch it (clearing the
/// buffer) or hold it (emitting a one-shot "waiting" decision).
fn try_dispatch(
    table: &Arc<Mutex<RouteTable>>,
    telem: &Telemetry,
    buffer: &mut Vec<KeyTap>,
    announced_wait: &mut bool,
) {
    let (choice, reason, candidates) = {
        let t = table.lock().unwrap();
        let (c, r) = t.choose();
        (c, r, t.info_list())
    };

    match choice {
        Some(route_id) => {
            telem.emit(TelemetryKind::RouteDecision {
                route: Some(route_id.clone()),
                reason,
                candidates,
            });
            dispatch(table, telem, &route_id, buffer);
            buffer.clear();
            *announced_wait = false;
        }
        None => {
            // Hold the buffer until a route frees up; announce once.
            if !*announced_wait {
                telem.emit(TelemetryKind::RouteDecision {
                    route: None,
                    reason,
                    candidates,
                });
                *announced_wait = true;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tap(hid: u8) -> KeyTap {
        KeyTap::new(1, 0, 0, hid)
    }

    #[test]
    fn auto_prefers_higher_priority_eligible() {
        let t = RouteTable::default_exhibit();
        // game is gated_on_ready and starts not-ready, trophy is ready → trophy wins.
        let (choice, _) = t.choose();
        assert_eq!(choice.as_deref(), Some("trophy"));
    }

    #[test]
    fn game_wins_once_ready() {
        let mut t = RouteTable::default_exhibit();
        t.set_status("game", Some(false), Some(true));
        let (choice, _) = t.choose();
        assert_eq!(choice.as_deref(), Some("game"));
    }

    #[test]
    fn busy_routes_are_skipped() {
        let mut t = RouteTable::default_exhibit();
        t.set_status("game", Some(true), Some(true));
        t.set_status("trophy", Some(true), None);
        let (choice, reason) = t.choose();
        assert!(choice.is_none());
        assert!(reason.contains("no eligible"));
    }

    #[test]
    fn pin_overrides_priority() {
        let mut t = RouteTable::default_exhibit();
        t.set_status("game", Some(false), Some(true));
        t.override_active = Some("trophy".into());
        let (choice, _) = t.choose();
        assert_eq!(choice.as_deref(), Some("trophy"));
    }

    #[test]
    fn pinned_busy_route_holds() {
        let mut t = RouteTable::default_exhibit();
        t.override_active = Some("trophy".into());
        t.set_status("trophy", Some(true), None);
        let (choice, reason) = t.choose();
        assert!(choice.is_none());
        assert!(reason.contains("busy"));
    }

    #[test]
    fn disabled_holds_everything() {
        let mut t = RouteTable::default_exhibit();
        t.enabled = false;
        let (choice, _) = t.choose();
        assert!(choice.is_none());
    }

    #[test]
    fn decode_buffer_builds_text() {
        let taps = vec![tap(0x04), tap(0x2c), tap(0x05), tap(0x28)];
        assert_eq!(decode_buffer(&taps), "a b");
    }

    #[test]
    fn unknown_status_route_is_ignored() {
        let mut t = RouteTable::default_exhibit();
        assert!(!t.set_status("nope", Some(true), None));
    }
}
