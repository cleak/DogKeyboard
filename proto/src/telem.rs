//! DOGKBD telemetry protocol (router → Pi display).
//!
//! A second, reverse channel that carries everything the router knows about what
//! Momo is doing and where it routed her input. Sent as **newline-delimited JSON**,
//! one [`TelemetryEvent`] per UDP datagram (default port 44556), broadcast and
//! duplicate-sent like [`crate::KeyTap`]. The Pi-side display owns all rendering;
//! the router only reports facts.
//!
//! Each event carries a monotonic `seq` (per router run) and a `run_id`, so the
//! display can de-duplicate the duplicate-sends and detect drops via `seq` gaps.

use serde::{Deserialize, Serialize};

/// Telemetry protocol version (independent of the KeyTap wire version).
pub const TELEM_VERSION: u32 = 1;

/// Default UDP port for the telemetry / display channel.
pub const TELEM_PORT: u16 = 44556;

/// Max length of free-text fields, so a single event always fits one datagram.
pub const TELEM_TEXT_CAP: usize = 600;

/// A logical route the router can send Momo's input to.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RouteInfo {
    /// Stable id, e.g. `"trophy"` or `"game"`.
    pub id: String,
    /// Human label for the display, e.g. `"Trophy Factory"`.
    pub label: String,
    /// Claude Code instance is currently working (not accepting input).
    pub busy: bool,
    /// Destination reports it is ready to accept input (e.g. game in an input state).
    pub ready: bool,
}

/// How a single received keystroke was handled.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Disposition {
    /// Accepted into the input buffer / streamed live.
    Accepted,
    /// Rejected by the allowlist (should be rare; sender also filters).
    Blocked,
    /// Duplicate of a packet already seen (UDP duplicate-send or retransmit).
    Dup,
}

/// Why a keystroke or packet was dropped rather than buffered.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DropReason {
    Disarmed,
    NoRoute,
    Blocked,
    Dedup,
    TargetNotForeground,
}

/// The payload of a telemetry event. Tagged by `kind`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TelemetryKind {
    /// Router start / periodic heartbeat. Lets the display show liveness and the
    /// full route table even if it joined mid-stream.
    Hello {
        routes: Vec<RouteInfo>,
        /// Currently active route id, or `None` when waiting / auto.
        active: Option<String>,
    },

    /// A single key Momo pressed. Streamed live for the keystroke panel.
    Keystroke {
        disposition: Disposition,
        hid: u8,
        shift: bool,
        /// Decoded form: a printable char, or `"SPACE"`/`"ENTER"`/`"BACKSPACE"`.
        decoded: String,
        device_id: u32,
    },

    /// The current input buffer (decoded text) changed.
    Buffer {
        /// Route the buffer is currently aimed at (if decided), else `None`.
        route: Option<String>,
        text: String,
        len: usize,
    },

    /// A key/packet was discarded.
    Drop {
        reason: DropReason,
        #[serde(skip_serializing_if = "Option::is_none")]
        hid: Option<u8>,
    },

    /// A burst is about to be sent; records the decision and the candidates.
    RouteDecision {
        /// Chosen route id, or `None` when holding ("waiting").
        route: Option<String>,
        reason: String,
        candidates: Vec<RouteInfo>,
    },

    /// Input was actually injected into a destination window.
    Dispatch {
        route: String,
        text: String,
        chars: usize,
        auto_enter: bool,
    },

    /// A route's busy/ready state changed (from a status post).
    RouteStatus {
        route: String,
        busy: bool,
        ready: bool,
        /// Where the update came from, e.g. `"claude-hook"`, `"game-bridge"`.
        source: String,
    },

    /// Trophy progress, posted by the trophy monitor.
    Trophy {
        /// The trophy run folder id. Named distinctly so it does not collide
        /// with the flattened envelope `run_id`.
        #[serde(rename = "trophy_run")]
        trophy_run: String,
        award_title: String,
        family: String,
        topper: String,
        /// The raw keyboard burst that produced this trophy.
        source_burst: String,
        /// e.g. `"compiling"`, `"complete"`.
        status: String,
        created_at: String,
    },

    /// Game state, posted by the game bridge.
    Game {
        state: String,
        phase: String,
        wave: i64,
        integrity: f64,
        score: i64,
        ready: bool,
        /// Last burst routed to the game (raw chars), if any.
        last_input: String,
        /// Decoded / human-readable form of that input.
        last_input_decoded: String,
    },
}

/// A telemetry event with its envelope. One per UDP datagram (JSON + `\n`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TelemetryEvent {
    pub v: u32,
    pub run_id: u32,
    pub seq: u64,
    pub ts_ms: u64,
    #[serde(flatten)]
    pub kind: TelemetryKind,
}

impl TelemetryEvent {
    pub fn new(run_id: u32, seq: u64, ts_ms: u64, kind: TelemetryKind) -> Self {
        Self {
            v: TELEM_VERSION,
            run_id,
            seq,
            ts_ms,
            kind,
        }
    }

    /// Serialize to a single newline-terminated JSON line for the wire.
    pub fn to_line(&self) -> String {
        let mut s = serde_json::to_string(self).unwrap_or_else(|_| "{}".to_string());
        s.push('\n');
        s
    }

    /// Parse from a wire line (trailing newline optional).
    pub fn from_line(line: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(line.trim_end())
    }
}

/// Truncate free text to [`TELEM_TEXT_CAP`] on a char boundary, so a single event
/// always fits one UDP datagram.
pub fn cap_text(s: &str) -> String {
    if s.len() <= TELEM_TEXT_CAP {
        return s.to_string();
    }
    let mut end = TELEM_TEXT_CAP;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    let mut out = s[..end].to_string();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_keystroke() {
        let ev = TelemetryEvent::new(
            7,
            42,
            1_749_700_000_123,
            TelemetryKind::Keystroke {
                disposition: Disposition::Accepted,
                hid: 0x04,
                shift: false,
                decoded: "a".to_string(),
                device_id: 0xdead_beef,
            },
        );
        let line = ev.to_line();
        assert!(line.ends_with('\n'));
        let back = TelemetryEvent::from_line(&line).unwrap();
        assert_eq!(ev, back);
    }

    #[test]
    fn kind_is_tagged() {
        let ev = TelemetryEvent::new(1, 1, 0, TelemetryKind::Hello { routes: vec![], active: None });
        let line = ev.to_line();
        assert!(line.contains(r#""kind":"hello""#));
        // envelope is flattened, not nested
        assert!(line.contains(r#""seq":1"#));
    }

    #[test]
    fn drop_omits_none_hid() {
        let ev = TelemetryEvent::new(
            1,
            1,
            0,
            TelemetryKind::Drop { reason: DropReason::NoRoute, hid: None },
        );
        let line = ev.to_line();
        assert!(!line.contains("hid"));
        assert!(line.contains(r#""reason":"no_route""#));
    }

    #[test]
    fn trophy_run_does_not_collide_with_envelope_run_id() {
        let ev = TelemetryEvent::new(
            0xaabb,
            5,
            0,
            TelemetryKind::Trophy {
                trophy_run: "20260531T063307Z_overnigh".into(),
                award_title: "SUPREME CHAIRDOG".into(),
                family: "obelisk".into(),
                topper: "crown".into(),
                source_burst: "asdf".into(),
                status: "complete".into(),
                created_at: "now".into(),
            },
        );
        let line = ev.to_line();
        assert!(line.contains(r#""run_id":43707"#), "envelope run_id present: {line}");
        assert!(line.contains(r#""trophy_run":"20260531T063307Z_overnigh""#));
        let back = TelemetryEvent::from_line(&line).unwrap();
        assert_eq!(back.run_id, 0xaabb); // envelope run_id intact after roundtrip
        assert_eq!(ev, back);
    }

    #[test]
    fn cap_text_truncates_on_boundary() {
        let long = "é".repeat(500); // 1000 bytes
        let capped = cap_text(&long);
        assert!(capped.len() <= TELEM_TEXT_CAP + 4);
        assert!(capped.ends_with('…'));
        // short strings pass through unchanged
        assert_eq!(cap_text("hi"), "hi");
    }
}
