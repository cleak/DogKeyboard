//! DOGKBD Receiver GUI application

use crate::keys::KeyPreview;
use crate::target::WindowInfo;
use dogkbd_proto::KeyTap;
use eframe::egui;
use rodio::{Decoder, OutputStream, Sink};
use serde::Serialize;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Maximum number of key previews to show
const MAX_PREVIEW_KEYS: usize = 50;

/// Idle timeout for auto-enter and sequence reset (6 seconds)
const IDLE_TIMEOUT_SECS: u64 = 5;

/// Seconds to wait after thresholds are met before dispensing treat
const TREAT_DELAY_SECS: u64 = 2;

/// Seconds to wait in busy state before backspacing out buffered text
const BUSY_BACKSPACE_TIMEOUT_SECS: u64 = 20;

/// Grace period before confirming a busy→idle transition (debounce thrashing)
const IDLE_TRANSITION_DELAY_SECS: u64 = 2;

/// Minimum total text characters required to submit (auto-enter) and dispense treat
const VALIDATION_CHAR_THRESHOLD: usize = 16;

/// Minimum text characters typed while Claude is idle
const IDLE_CHAR_THRESHOLD: usize = 4;

// --- Collection mode tuning ---
/// Minimum characters before a prompt is eligible to save on idle. Shorter
/// buffers keep accumulating across pauses — they are never discarded.
const COLLECT_MIN_CHARS: usize = 9;
/// At/above this many characters the prompt is considered "long" and is saved
/// after a shorter idle gap (nudges the dog on to the next prompt).
const COLLECT_LONG_CHARS: usize = 16;
/// Idle seconds before saving a long (16+ char) prompt.
const COLLECT_IDLE_SHORT_SECS: u64 = 2;
/// Idle seconds before saving a normal (9-15 char) prompt.
const COLLECT_IDLE_LONG_SECS: u64 = 5;
/// Default collection target shown in the UI.
const COLLECT_DEFAULT_TARGET: u64 = 100;
/// Number of recently-saved prompts to keep for the UI list.
const RECENT_PROMPTS_SHOWN: usize = 10;

/// Operating mode. The two modes are mutually exclusive: Normal injects to a
/// target window with the auto-enter/treat machinery; Collect purely records
/// the dog's prompts to a file with no injection or treats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AppMode {
    Normal,
    Collect,
}

/// One saved prompt, serialized as a single JSONL line.
#[derive(Serialize)]
struct CollectRecord<'a> {
    /// Unix epoch milliseconds at save time.
    ts_ms: u64,
    /// 1-based sequence number within the output file.
    seq: u64,
    /// The text the dog typed for this prompt.
    text: &'a str,
}

/// Current Unix time in milliseconds (0 if the clock is before the epoch).
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Filename for a fresh collection session: `dogkbd_prompts_<local-timestamp>.jsonl`.
/// Each app launch gets its own file so sessions never share data.
fn default_session_path() -> String {
    let ts = chrono::Local::now().format("%Y-%m-%d_%H-%M-%S");
    format!("dogkbd_prompts_{}.jsonl", ts)
}

/// Application state
pub struct DogkbdApp {
    /// Channel to receive key taps from network thread
    rx: Receiver<KeyTap>,
    /// Whether injection is armed
    armed: bool,
    /// Selected target window
    target_window: Option<WindowInfo>,
    /// Available windows for targeting
    available_windows: Vec<WindowInfo>,
    /// Recent key previews
    key_preview: Vec<KeyPreview>,
    /// Status message
    status: String,
    /// Error message
    error: Option<String>,
    /// Count of received packets
    packet_count: u64,
    /// Count of injected keys
    inject_count: u64,

    // Feature 1: Auto-enter on idle
    /// Whether auto-enter on idle is enabled
    auto_enter_on_idle: bool,
    /// Whether we've received input since the last Enter
    has_input_since_enter: bool,
    /// Time of last input received
    last_input_time: Option<Instant>,

    // Feature 2: Validation tone
    /// Whether validation tone is enabled
    validation_tone_enabled: bool,
    /// Previous state of validation_tone_enabled (for toggle detection)
    prev_validation_tone_enabled: bool,
    /// Count of text characters in current input sequence (total)
    text_char_count: usize,
    /// Count of text characters typed while Claude is idle
    idle_char_count: usize,
    /// Whether the validation tone has been played for this sequence
    validation_tone_played: bool,
    /// Whether a chime is pending (deferred until busy→idle transition)
    chime_pending: bool,
    /// Scheduled treat dispense time (set when thresholds met, fires after delay)
    treat_dispense_at: Option<Instant>,
    /// Audio output stream (must be kept alive for audio playback to work)
    #[allow(dead_code)]
    audio_stream: Option<OutputStream>,
    /// Audio sink for playing sounds
    audio_sink: Option<Sink>,
    /// Whether audio is available (for UI feedback)
    audio_available: bool,

    // Feature 3: Periodic auto-enter
    /// Whether periodic auto-enter is enabled
    periodic_enter_enabled: bool,
    /// Previous state of periodic_enter_enabled (for toggle detection)
    prev_periodic_enter_enabled: bool,
    /// Interval in seconds for periodic auto-enter
    periodic_enter_interval: u64,
    /// Previous interval value (for change detection)
    prev_periodic_enter_interval: u64,
    /// Time of last periodic enter injection
    last_periodic_enter: Instant,
    /// Flag to prevent double-enter in same frame
    enter_injected_this_frame: bool,

    // Input delay
    /// Delay in milliseconds before processing received keystrokes
    input_delay_ms: u64,
    /// Buffer of delayed key taps: (ready_at, tap)
    delay_buffer: VecDeque<(Instant, KeyTap)>,

    // Claude Code busy state
    /// Whether Claude Code is currently processing (set via HTTP endpoint)
    claude_busy: Arc<AtomicBool>,
    /// Whether Claude was busy last frame (for transition detection)
    claude_was_busy: bool,
    /// Whether a treat has been dispensed during the current Claude idle period.
    /// Only resets on a busy→idle transition — prevents double-dispensing.
    treat_dispensed_this_idle: bool,

    // Busy-state text cleanup
    /// Scheduled time to backspace out buffered text during busy state
    busy_backspace_at: Option<Instant>,

    // Idle transition debounce
    /// Pending busy→idle transition time (grace period to absorb thrashing)
    idle_transition_at: Option<Instant>,

    // Collection mode (pure capture)
    /// Current operating mode (mutually exclusive with Normal).
    mode: AppMode,
    /// Accumulating text of the dog's in-progress prompt.
    current_prompt: String,
    /// Output file path for collected prompts (JSONL, append).
    collect_path: String,
    /// Prompts written to the current file (seeded from the file on entry).
    collected_count: u64,
    /// Display-only target number of prompts.
    collect_target: u64,
    /// Ring buffer of recently saved prompts for the UI.
    recent_prompts: VecDeque<String>,
}

impl DogkbdApp {
    pub fn new(rx: Receiver<KeyTap>, claude_busy: Arc<AtomicBool>) -> Self {
        // Initialize audio output
        let (audio_stream, audio_sink, audio_available) = match OutputStream::try_default() {
            Ok((stream, handle)) => match Sink::try_new(&handle) {
                Ok(sink) => (Some(stream), Some(sink), true),
                Err(_) => (None, None, false),
            },
            Err(_) => (None, None, false),
        };

        let mut app = Self {
            rx,
            armed: false,
            target_window: None,
            available_windows: Vec::new(),
            key_preview: Vec::new(),
            status: if audio_available {
                "Ready".to_string()
            } else {
                "Ready (audio unavailable)".to_string()
            },
            error: None,
            packet_count: 0,
            inject_count: 0,

            // Feature 1: Auto-enter on idle (disabled by default)
            auto_enter_on_idle: true,
            has_input_since_enter: false,
            last_input_time: None,

            // Feature 2: Validation tone (enabled by default)
            validation_tone_enabled: true,
            prev_validation_tone_enabled: true,
            text_char_count: 0,
            idle_char_count: 0,
            validation_tone_played: false,
            chime_pending: false,
            treat_dispense_at: None,
            audio_stream,
            audio_sink,
            audio_available,

            // Feature 3: Periodic auto-enter (enabled by default, 15s)
            periodic_enter_enabled: true,
            prev_periodic_enter_enabled: true,
            periodic_enter_interval: 15,
            prev_periodic_enter_interval: 15,
            last_periodic_enter: Instant::now(),
            enter_injected_this_frame: false,

            // Input delay (100ms by default)
            input_delay_ms: 100,
            delay_buffer: VecDeque::new(),

            // Claude Code busy state (idle by default)
            claude_busy,
            claude_was_busy: false,
            treat_dispensed_this_idle: false,

            // Busy-state text cleanup
            busy_backspace_at: None,

            // Idle transition debounce
            idle_transition_at: None,

            // Collection mode (Normal by default — preserves existing behavior)
            mode: AppMode::Normal,
            current_prompt: String::new(),
            collect_path: default_session_path(),
            collected_count: 0,
            collect_target: COLLECT_DEFAULT_TARGET,
            recent_prompts: VecDeque::new(),
        };
        app.refresh_windows();
        app
    }

    /// Refresh the list of available windows
    fn refresh_windows(&mut self) {
        #[cfg(windows)]
        {
            self.available_windows = crate::target::enumerate_windows();
        }
        #[cfg(not(windows))]
        {
            self.available_windows.clear();
        }
    }

    /// Whether Claude is effectively busy (raw busy OR in idle-transition grace period)
    fn is_effectively_busy(&self) -> bool {
        self.claude_busy.load(Ordering::Relaxed) || self.idle_transition_at.is_some()
    }

    /// Play the chime sound (indicates ready for more input)
    fn play_validation_tone(&self) {
        static CHIME_BYTES: &[u8] = include_bytes!("../chime.mp3");
        if let Some(ref sink) = self.audio_sink {
            let cursor = std::io::Cursor::new(CHIME_BYTES);
            if let Ok(source) = Decoder::new(cursor) {
                sink.append(source);
            }
        }
    }

    /// Dispense a dog treat via SSH command
    fn dispense_treat() {
        std::thread::spawn(|| {
            match std::process::Command::new("ssh")
                .args(["caleb@zoltan", "/usr/local/bin/treat1"])
                .spawn()
            {
                Ok(mut child) => {
                    let _ = child.wait();
                }
                Err(e) => {
                    eprintln!("Failed to dispense treat: {}", e);
                }
            }
        });
    }

    /// Inject an Enter key (auto-generated, not from keyboard)
    fn inject_enter(&mut self) -> bool {
        // Prevent double-enter in same frame
        if self.enter_injected_this_frame {
            return false;
        }

        if !self.armed {
            return false;
        }

        if let Some(ref target) = self.target_window {
            #[cfg(windows)]
            let is_fg = crate::target::is_foreground(target.hwnd);
            #[cfg(not(windows))]
            let is_fg = true;

            if is_fg {
                // Create an Enter key tap (HID code 0x28)
                let enter_tap = KeyTap::new(0, 0, 0, 0x28);
                match crate::inject::inject(&enter_tap) {
                    Ok(()) => {
                        self.inject_count += 1;
                        self.error = None;
                        self.enter_injected_this_frame = true;
                        // Add to preview (marked as auto-enter)
                        self.key_preview.push(KeyPreview::AutoEnter);
                        if self.key_preview.len() > MAX_PREVIEW_KEYS {
                            self.key_preview.remove(0);
                        }
                        // Reset input tracking since we just sent an Enter
                        self.has_input_since_enter = false;
                        return true;
                    }
                    Err(e) => {
                        self.error = Some(e);
                    }
                }
            }
        }
        false
    }

    /// Inject N backspace keys to clear buffered text from target window
    fn inject_backspaces(&mut self, count: usize) -> usize {
        if !self.armed || count == 0 {
            return 0;
        }
        if let Some(ref target) = self.target_window {
            #[cfg(windows)]
            let is_fg = crate::target::is_foreground(target.hwnd);
            #[cfg(not(windows))]
            let is_fg = true;

            if is_fg {
                let mut injected = 0;
                for _ in 0..count {
                    let bs_tap = KeyTap::new(0, 0, 0, 0x2a);
                    match crate::inject::inject(&bs_tap) {
                        Ok(()) => {
                            self.inject_count += 1;
                            injected += 1;
                        }
                        Err(e) => {
                            self.error = Some(e);
                            break;
                        }
                    }
                }
                return injected;
            }
        }
        0
    }

    /// Check if buffered text should be backspaced out during busy state
    fn check_busy_backspace(&mut self) {
        if self.mode != AppMode::Normal {
            return;
        }
        if !self.is_effectively_busy() {
            return;
        }
        if let Some(at) = self.busy_backspace_at {
            if Instant::now() >= at {
                let count = self.text_char_count;
                if count > 0 {
                    println!(
                        "[busy-backspace] Backspacing {} chars after {}s busy timeout",
                        count, BUSY_BACKSPACE_TIMEOUT_SECS
                    );
                    let injected = self.inject_backspaces(count);
                    for _ in 0..injected {
                        self.key_preview.push(KeyPreview::Backspace);
                        if self.key_preview.len() > MAX_PREVIEW_KEYS {
                            self.key_preview.remove(0);
                        }
                    }
                    self.status = format!("Backspaced {} chars (busy timeout)", injected);
                }
                // Reset text tracking
                self.text_char_count = 0;
                self.idle_char_count = 0;
                self.has_input_since_enter = false;
                self.validation_tone_played = false;
                self.treat_dispense_at = None;
                self.busy_backspace_at = None;
                println!("[busy-backspace] Done, reset text tracking");
            }
        }
    }

    /// Check if a key preview is a text character (for validation tone counting)
    fn is_text_char(preview: &KeyPreview) -> bool {
        matches!(preview, KeyPreview::Char(_) | KeyPreview::Space)
    }

    /// Process pending key taps (with configurable delay)
    fn process_keys(&mut self) {
        // Stage 1: Drain channel into delay buffer
        let delay = Duration::from_millis(self.input_delay_ms);
        let now = Instant::now();
        while let Ok(tap) = self.rx.try_recv() {
            self.packet_count += 1;
            self.delay_buffer.push_back((now + delay, tap));
        }

        // Stage 2: Process taps whose delay has elapsed
        while let Some(&(ready_at, _)) = self.delay_buffer.front() {
            if Instant::now() < ready_at {
                break;
            }
            let (_, tap) = self.delay_buffer.pop_front().unwrap();
            self.process_single_tap(&tap);
        }
    }

    /// Process a single key tap (preview, treat dispense, injection)
    fn process_single_tap(&mut self, tap: &KeyTap) {
        // Filter out Enter key entirely (filtered in net.rs too, this is a safety belt)
        if tap.hid_code == 0x28 {
            return;
        }

        // Collection mode: pure capture. Accumulate regardless of arm state —
        // nothing is injected, so there's no reason to gate saving behind ARM.
        if self.mode == AppMode::Collect {
            self.last_input_time = Some(Instant::now());
            self.process_tap_collect(tap);
            return;
        }

        // Normal mode: ignore all input when not armed (keys would be injected).
        if !self.armed {
            return;
        }

        // Track input timing
        self.last_input_time = Some(Instant::now());

        // Add to preview
        if let Some(preview) = KeyPreview::from_tap(tap) {
            self.has_input_since_enter = true;

            // Track text characters for treat threshold
            let claude_idle = !self.is_effectively_busy();
            // Backspace decrements count (but not below 0)
            if matches!(preview, KeyPreview::Backspace) {
                self.text_char_count = self.text_char_count.saturating_sub(1);
                if claude_idle {
                    self.idle_char_count = self.idle_char_count.saturating_sub(1);
                }
            } else if Self::is_text_char(&preview) {
                self.text_char_count += 1;
                if claude_idle {
                    self.idle_char_count += 1;
                }

                // Schedule treat when both thresholds reached (not yet dispensed this idle period)
                if self.text_char_count >= VALIDATION_CHAR_THRESHOLD
                    && self.idle_char_count >= IDLE_CHAR_THRESHOLD
                    && !self.validation_tone_played
                    && !self.treat_dispensed_this_idle
                    && self.treat_dispense_at.is_none()
                    && self.armed
                    && claude_idle
                {
                    println!(
                        "[treat] Scheduling treat in {}s (chars: {}/{}, idle_chars: {}/{}, dispensed_this_idle: {})",
                        TREAT_DELAY_SECS, self.text_char_count, VALIDATION_CHAR_THRESHOLD,
                        self.idle_char_count, IDLE_CHAR_THRESHOLD, self.treat_dispensed_this_idle
                    );
                    self.treat_dispense_at =
                        Some(Instant::now() + Duration::from_secs(TREAT_DELAY_SECS));
                }
            }

            self.key_preview.push(preview);
            if self.key_preview.len() > MAX_PREVIEW_KEYS {
                self.key_preview.remove(0);
            }

            // Schedule backspace if typing during busy state with no pending backspace
            if self.is_effectively_busy()
                && self.has_input_since_enter
                && self.busy_backspace_at.is_none()
            {
                println!(
                    "[busy-backspace] Scheduling backspace in {}s (new input during busy, text_char_count: {})",
                    BUSY_BACKSPACE_TIMEOUT_SECS, self.text_char_count
                );
                self.busy_backspace_at =
                    Some(Instant::now() + Duration::from_secs(BUSY_BACKSPACE_TIMEOUT_SECS));
            }
        }

        // Inject if armed and target window is foreground
        if self.armed {
            if let Some(ref target) = self.target_window {
                #[cfg(windows)]
                let is_fg = crate::target::is_foreground(target.hwnd);
                #[cfg(not(windows))]
                let is_fg = true; // Linux injects to focused window

                if is_fg {
                    match crate::inject::inject(tap) {
                        Ok(()) => {
                            self.inject_count += 1;
                            self.error = None;
                        }
                        Err(e) => {
                            self.error = Some(e);
                        }
                    }
                } else {
                    self.status = "Target not foreground".to_string();
                }
            }
        }
    }

    /// Check and handle idle timeout (auto-enter and sequence reset)
    fn check_idle_timeout(&mut self) {
        if self.mode != AppMode::Normal {
            return;
        }
        // Don't auto-enter while Claude Code is processing (or in grace period)
        if self.is_effectively_busy() {
            return;
        }

        if let Some(last_input) = self.last_input_time {
            let idle_duration = last_input.elapsed();

            if idle_duration >= Duration::from_secs(IDLE_TIMEOUT_SECS) {
                // Auto-enter on idle if enabled, has input, and minimum chars met
                if self.auto_enter_on_idle
                    && self.has_input_since_enter
                    && self.text_char_count >= VALIDATION_CHAR_THRESHOLD
                    && self.idle_char_count >= IDLE_CHAR_THRESHOLD
                {
                    println!(
                        "[idle] Auto-enter firing (chars: {}, idle_chars: {}, treat_dispensed_this_idle: {})",
                        self.text_char_count, self.idle_char_count, self.treat_dispensed_this_idle
                    );
                    if self.inject_enter() {
                        self.status = "Auto-enter injected (idle timeout)".to_string();
                        if self.validation_tone_enabled {
                            self.chime_pending = true;
                        }
                    }
                    // Reset sequence counters after submission
                    self.text_char_count = 0;
                    self.idle_char_count = 0;
                    self.validation_tone_played = false;
                } else if self.has_input_since_enter {
                    println!(
                        "[idle] Keyboard idle {}s but thresholds not met (chars: {}/{}, idle_chars: {}/{}, auto_enter: {})",
                        idle_duration.as_secs(), self.text_char_count, VALIDATION_CHAR_THRESHOLD,
                        self.idle_char_count, IDLE_CHAR_THRESHOLD, self.auto_enter_on_idle
                    );
                }

                // Clear last_input_time to prevent repeated triggers
                self.last_input_time = None;
            }
        }
    }

    /// Check and handle periodic auto-enter (only during busy state)
    fn check_periodic_enter(&mut self) {
        if self.mode != AppMode::Normal {
            return;
        }
        // Only periodic-enter while Claude Code is effectively busy
        if !self.is_effectively_busy() {
            return;
        }

        // Don't send periodic enter if there's buffered text (wait for backspace-out first)
        if self.has_input_since_enter {
            return;
        }

        if self.periodic_enter_enabled {
            let elapsed = self.last_periodic_enter.elapsed();
            if elapsed >= Duration::from_secs(self.periodic_enter_interval) {
                println!("[periodic] Periodic auto-enter firing (busy state, no buffered text)");
                if self.inject_enter() {
                    self.status = "Periodic auto-enter injected (busy)".to_string();
                }
                self.last_periodic_enter = Instant::now();
            }
        }
    }

    /// Reset transient state when switching modes; seed the collect counter.
    fn on_mode_changed(&mut self) {
        // Clear in-progress buffers/timers from both pipelines.
        self.current_prompt.clear();
        self.key_preview.clear();
        self.last_input_time = None;
        self.has_input_since_enter = false;
        self.text_char_count = 0;
        self.idle_char_count = 0;
        self.treat_dispense_at = None;
        self.busy_backspace_at = None;
        self.idle_transition_at = None;
        self.validation_tone_played = false;
        self.chime_pending = false;

        match self.mode {
            AppMode::Collect => {
                self.collected_count = Self::count_existing_prompts(&self.collect_path);
                self.recent_prompts.clear();
                self.status = format!(
                    "Collect mode — {} prompt(s) already in {}",
                    self.collected_count, self.collect_path
                );
            }
            AppMode::Normal => {
                self.status = "Normal mode".to_string();
            }
        }
    }

    /// Count non-blank lines already present in the output file (0 if missing).
    fn count_existing_prompts(path: &str) -> u64 {
        std::fs::read_to_string(path)
            .map(|s| s.lines().filter(|l| !l.trim().is_empty()).count() as u64)
            .unwrap_or(0)
    }

    /// Accumulate one tap into the current prompt buffer (collection mode).
    fn process_tap_collect(&mut self, tap: &KeyTap) {
        if let Some(preview) = KeyPreview::from_tap(tap) {
            match preview {
                KeyPreview::Backspace => {
                    self.current_prompt.pop();
                }
                KeyPreview::Space => self.current_prompt.push(' '),
                KeyPreview::Char(c) => self.current_prompt.push(c),
                // Enter/AutoEnter never reach here (Enter is filtered upstream).
                KeyPreview::Enter | KeyPreview::AutoEnter => {}
            }
            self.key_preview.push(preview);
            if self.key_preview.len() > MAX_PREVIEW_KEYS {
                self.key_preview.remove(0);
            }
        }
    }

    /// Save the current prompt as one JSONL line, then clear the buffer.
    /// On a write error the buffer is preserved so data isn't lost.
    fn flush_collect_prompt(&mut self) {
        if self.current_prompt.is_empty() {
            return;
        }
        let line = match serde_json::to_string(&CollectRecord {
            ts_ms: now_ms(),
            seq: self.collected_count + 1,
            text: &self.current_prompt,
        }) {
            Ok(l) => l,
            Err(e) => {
                self.error = Some(format!("Prompt serialize failed: {}", e));
                return;
            }
        };

        use std::io::Write;
        let result = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.collect_path)
            .and_then(|mut f| writeln!(f, "{}", line));

        match result {
            Ok(()) => {
                self.collected_count += 1;
                self.error = None;
                let text = std::mem::take(&mut self.current_prompt);
                println!("[collect] Saved prompt #{}: {:?}", self.collected_count, text);
                self.recent_prompts.push_back(text);
                while self.recent_prompts.len() > RECENT_PROMPTS_SHOWN {
                    self.recent_prompts.pop_front();
                }
                self.status = format!("Collected {} prompt(s)", self.collected_count);
            }
            Err(e) => {
                self.error = Some(format!("Save failed ({}): {}", self.collect_path, e));
            }
        }
    }

    /// Save the current prompt once typing has been idle long enough. Long
    /// prompts (16+ chars) save after 2s; normal prompts (9+ chars) after 5s.
    /// Buffers under 9 chars never time out — they keep accumulating until they
    /// reach the threshold.
    fn check_collect_timeout(&mut self) {
        if self.mode != AppMode::Collect {
            return;
        }
        let last = match self.last_input_time {
            Some(t) => t,
            None => return,
        };
        if self.current_prompt.is_empty() {
            return;
        }

        let idle = last.elapsed();
        let len = self.current_prompt.chars().count();
        let save = (len >= COLLECT_LONG_CHARS
            && idle >= Duration::from_secs(COLLECT_IDLE_SHORT_SECS))
            || (len >= COLLECT_MIN_CHARS && idle >= Duration::from_secs(COLLECT_IDLE_LONG_SECS));

        if save {
            self.flush_collect_prompt();
            self.last_input_time = None;
        }
        // Buffers under COLLECT_MIN_CHARS are left untouched — they keep
        // accumulating across pauses until they reach the save threshold.
    }

    /// Render the collection-mode UI panel.
    fn collect_ui(&mut self, ui: &mut egui::Ui) {
        ui.heading("Collect Prompts");
        ui.label(
            "Pure capture: no injection, no treats. Each session writes its own timestamped file.",
        );
        ui.add_space(4.0);

        // Output file path
        ui.horizontal(|ui| {
            ui.label("File:");
            let resp = ui.add(
                egui::TextEdit::singleline(&mut self.collect_path)
                    .desired_width(ui.available_width() - 10.0),
            );
            if resp.changed() {
                self.collected_count = Self::count_existing_prompts(&self.collect_path);
            }
        });
        // Resolved absolute path (best-effort)
        let shown = std::fs::canonicalize(&self.collect_path)
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| {
                std::env::current_dir()
                    .map(|d| d.join(&self.collect_path).display().to_string())
                    .unwrap_or_else(|_| self.collect_path.clone())
            });
        ui.horizontal(|ui| {
            ui.add_space(20.0);
            ui.colored_label(egui::Color32::GRAY, shown);
        });

        ui.add_space(6.0);

        // Goal (purely a visual marker — never caps how many prompts are saved)
        ui.horizontal(|ui| {
            ui.label("Goal:");
            ui.add(egui::Slider::new(&mut self.collect_target, 10..=1000).suffix(" prompts"));
        });

        ui.add_space(6.0);

        // Progress (the goal is just a marker; collection is unbounded)
        let color = if self.collected_count >= self.collect_target {
            egui::Color32::GREEN
        } else {
            egui::Color32::LIGHT_BLUE
        };
        ui.colored_label(
            color,
            egui::RichText::new(format!(
                "Collected: {}   (goal: {})",
                self.collected_count, self.collect_target
            ))
            .heading(),
        );
        ui.colored_label(
            egui::Color32::GREEN,
            "● Capturing — no arming needed; every prompt is saved to the file automatically.",
        );

        ui.add_space(6.0);

        // Current in-progress prompt
        let len = self.current_prompt.chars().count();
        let window = if len >= COLLECT_LONG_CHARS {
            format!("saves after {}s idle", COLLECT_IDLE_SHORT_SECS)
        } else if len >= COLLECT_MIN_CHARS {
            format!("saves after {}s idle", COLLECT_IDLE_LONG_SECS)
        } else {
            format!("needs {}+ chars to save", COLLECT_MIN_CHARS)
        };
        ui.label(format!("Current prompt ({} chars — {}):", len, window));
        ui.add(
            egui::TextEdit::multiline(&mut self.current_prompt.as_str())
                .desired_width(f32::INFINITY)
                .desired_rows(2)
                .font(egui::TextStyle::Monospace),
        );

        ui.add_space(6.0);

        // Recently saved prompts
        ui.label("Recently saved:");
        egui::ScrollArea::vertical()
            .max_height(150.0)
            .id_salt("recent_prompts")
            .show(ui, |ui| {
                if self.recent_prompts.is_empty() {
                    ui.colored_label(egui::Color32::GRAY, "(none yet)");
                } else {
                    for (i, p) in self.recent_prompts.iter().rev().enumerate() {
                        ui.label(format!("{}. {}", self.collected_count - i as u64, p));
                    }
                }
            });
    }
}

impl eframe::App for DogkbdApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Reset per-frame flags
        self.enter_injected_this_frame = false;

        // Detect state transitions (with debounced busy→idle)
        if self.mode == AppMode::Normal {
        let claude_busy_now = self.claude_busy.load(Ordering::Relaxed);
        if let Some(at) = self.idle_transition_at {
            // In grace period — waiting to confirm busy→idle
            if claude_busy_now {
                // Claude went back to busy — cancel idle transition, stay busy
                println!(
                    "[state] Claude went busy during {}s idle grace period — staying busy",
                    IDLE_TRANSITION_DELAY_SECS
                );
                self.idle_transition_at = None;
            } else if Instant::now() >= at {
                // Grace period elapsed, truly idle — execute busy→idle transition
                println!(
                    "[state] Claude: busy → idle (confirmed after {}s, treat_dispensed_this_idle was {}, resetting to false)",
                    IDLE_TRANSITION_DELAY_SECS, self.treat_dispensed_this_idle
                );
                self.last_periodic_enter = Instant::now();
                self.treat_dispensed_this_idle = false;
                self.busy_backspace_at = None;
                if self.validation_tone_enabled {
                    println!("[state] Playing chime on busy→idle transition");
                    self.play_validation_tone();
                }
                self.chime_pending = false;
                // Bring target window to foreground so dog's next keystrokes land there
                #[cfg(windows)]
                if let Some(ref target) = self.target_window {
                    println!("[state] Focusing target window: {}", target.display_name());
                    crate::target::set_foreground(target.hwnd);
                }
                self.idle_transition_at = None;
                self.claude_was_busy = false;
            }
            // else: still in grace period, wait
        } else if !claude_busy_now && self.claude_was_busy {
            // Raw busy→idle detected — start grace period
            println!(
                "[state] Claude raw busy → idle, starting {}s grace period",
                IDLE_TRANSITION_DELAY_SECS
            );
            self.idle_transition_at =
                Some(Instant::now() + Duration::from_secs(IDLE_TRANSITION_DELAY_SECS));
            // Don't update claude_was_busy — stays true during grace period
        } else if claude_busy_now && !self.claude_was_busy {
            // idle → busy transition
            println!("[state] Claude: idle → busy (resetting idle_char_count from {})", self.idle_char_count);
            self.idle_char_count = 0;
            // Schedule backspace if there's buffered text
            if self.has_input_since_enter && self.busy_backspace_at.is_none() {
                println!(
                    "[state] Scheduling busy backspace in {}s (text_char_count: {})",
                    BUSY_BACKSPACE_TIMEOUT_SECS, self.text_char_count
                );
                self.busy_backspace_at =
                    Some(Instant::now() + Duration::from_secs(BUSY_BACKSPACE_TIMEOUT_SECS));
            }
            self.claude_was_busy = claude_busy_now;
        }

        }

        // Process any pending keys
        self.process_keys();

        // Check if scheduled treat dispense is ready
        if let Some(at) = self.treat_dispense_at {
            if Instant::now() >= at {
                println!("[treat] Dispensing treat now! (treat_dispensed_this_idle: {} → true)", self.treat_dispensed_this_idle);
                Self::dispense_treat();
                self.validation_tone_played = true;
                self.treat_dispensed_this_idle = true;
                self.treat_dispense_at = None;
            }
        }

        // Check timer-based features
        self.check_busy_backspace();
        self.check_idle_timeout();
        self.check_periodic_enter();
        self.check_collect_timeout();

        // Request repaint to check for new keys and timers
        ctx.request_repaint_after(Duration::from_millis(16));

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("DOGKBD Receiver");
            ui.separator();

            // Status bar
            ui.horizontal(|ui| {
                ui.label(format!("Packets: {}", self.packet_count));
                ui.separator();
                ui.label(format!("Injected: {}", self.inject_count));
                ui.separator();
                if self.idle_transition_at.is_some() {
                    ui.colored_label(egui::Color32::YELLOW, "Claude: settling...");
                } else if self.is_effectively_busy() {
                    ui.colored_label(egui::Color32::YELLOW, "Claude: busy");
                } else {
                    ui.colored_label(egui::Color32::GREEN, "Claude: idle");
                }
                ui.separator();
                ui.label(&self.status);
            });

            if let Some(ref err) = self.error {
                ui.colored_label(egui::Color32::RED, format!("Error: {}", err));
            }

            ui.separator();

            // Arm toggle (Normal mode only — Collect captures without arming)
            if self.mode == AppMode::Normal {
            ui.horizontal(|ui| {
                let arm_text = if self.armed { "ARMED" } else { "DISARMED" };
                let arm_color = if self.armed {
                    egui::Color32::RED
                } else {
                    egui::Color32::GRAY
                };

                if ui
                    .add(egui::Button::new(arm_text).fill(arm_color))
                    .clicked()
                {
                    self.armed = !self.armed;
                    self.status = if self.armed {
                        "Armed - keys will be injected".to_string()
                    } else {
                        "Disarmed - keys will not be injected".to_string()
                    };
                }

                if ui.button("Refresh Windows").clicked() {
                    self.refresh_windows();
                }

                if self.audio_available && ui.button("Chime").clicked() {
                    self.play_validation_tone();
                }

                if ui.button("Reset").clicked() {
                    println!("[reset] Manual state reset");
                    self.text_char_count = 0;
                    self.idle_char_count = 0;
                    self.validation_tone_played = false;
                    self.treat_dispensed_this_idle = false;
                    self.chime_pending = false;
                    self.has_input_since_enter = false;
                    self.last_input_time = None;
                    self.last_periodic_enter = Instant::now();
                    self.treat_dispense_at = None;
                    self.busy_backspace_at = None;
                    self.idle_transition_at = None;
                    self.key_preview.clear();
                    self.status = "State reset".to_string();
                }
            });
            } // end arm toggle (Normal mode only)

            ui.separator();

            // Mode selector (Normal injection vs. Collect prompts — mutually exclusive)
            ui.horizontal(|ui| {
                ui.label("Mode:");
                let old_mode = self.mode;
                ui.selectable_value(&mut self.mode, AppMode::Normal, "Normal (inject)");
                ui.selectable_value(&mut self.mode, AppMode::Collect, "Collect prompts");
                if self.mode != old_mode {
                    self.on_mode_changed();
                }
            });

            ui.separator();

            if self.mode == AppMode::Collect {
                self.collect_ui(ui);
            } else {

            // Window selection
            ui.label("Target Window:");
            egui::ComboBox::from_id_salt("target_window")
                .width(ui.available_width() - 10.0)
                .selected_text(
                    self.target_window
                        .as_ref()
                        .map(|w| w.display_name())
                        .unwrap_or_else(|| "None".to_string()),
                )
                .show_ui(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .max_height(300.0)
                        .show(ui, |ui| {
                            if ui.selectable_label(self.target_window.is_none(), "None").clicked() {
                                self.target_window = None;
                            }
                            for window in &self.available_windows {
                                let selected = self
                                    .target_window
                                    .as_ref()
                                    .map(|t| t.title == window.title)
                                    .unwrap_or(false);
                                if ui
                                    .selectable_label(selected, window.display_name())
                                    .clicked()
                                {
                                    self.target_window = Some(window.clone());
                                }
                            }
                        });
                });

            ui.separator();

            // Auto-features section (expanded by default)
            egui::CollapsingHeader::new("Auto Features")
                .default_open(true)
                .show(ui, |ui| {
                    // Input delay
                    ui.horizontal(|ui| {
                        ui.label("Input delay:");
                        ui.add(
                            egui::Slider::new(&mut self.input_delay_ms, 0..=5000)
                                .suffix(" ms")
                        );
                    });
                    if self.input_delay_ms > 0 && !self.delay_buffer.is_empty() {
                        ui.horizontal(|ui| {
                            ui.add_space(20.0);
                            ui.colored_label(
                                egui::Color32::YELLOW,
                                format!("{} key(s) buffered", self.delay_buffer.len()),
                            );
                        });
                    }

                    ui.add_space(4.0);

                    // Feature 1: Auto-enter on idle
                    ui.checkbox(&mut self.auto_enter_on_idle, "Auto-enter on idle (5s)")
                        .on_hover_text("Inject Enter after 5 seconds of no input (requires 10+ text characters)");

                    ui.add_space(4.0);

                    // Feature 2: Chime on submit
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut self.validation_tone_enabled, "Chime on submit")
                            .on_hover_text("Play a chime after auto-enter to indicate ready for more input");
                        if !self.audio_available {
                            ui.colored_label(egui::Color32::YELLOW, "(audio unavailable)");
                        } else if ui.small_button("Test").clicked() {
                            self.play_validation_tone();
                        }
                    });

                    // Detect toggle: reset state when re-enabled
                    if self.validation_tone_enabled && !self.prev_validation_tone_enabled {
                        self.text_char_count = 0;
                        self.idle_char_count = 0;
                        self.validation_tone_played = false;
                        self.chime_pending = false;
                    }
                    self.prev_validation_tone_enabled = self.validation_tone_enabled;

                    ui.horizontal(|ui| {
                        ui.add_space(20.0);
                        ui.label(format!(
                            "Chars: {}/{} (idle: {}/{})",
                            self.text_char_count, VALIDATION_CHAR_THRESHOLD,
                            self.idle_char_count, IDLE_CHAR_THRESHOLD,
                        ));
                        if self.chime_pending {
                            ui.colored_label(egui::Color32::YELLOW, "chime pending");
                        }
                        if self.treat_dispense_at.is_some() {
                            ui.colored_label(egui::Color32::YELLOW, "treat pending…");
                        } else if self.validation_tone_played {
                            ui.colored_label(egui::Color32::GREEN, "✓ Treat dispensed");
                        }
                    });

                    ui.add_space(4.0);

                    // Feature 3: Periodic auto-enter
                    ui.checkbox(&mut self.periodic_enter_enabled, "Periodic auto-enter")
                        .on_hover_text("Inject Enter at regular intervals");

                    // Detect toggle: reset timer when enabled
                    if self.periodic_enter_enabled && !self.prev_periodic_enter_enabled {
                        self.last_periodic_enter = Instant::now();
                    }
                    self.prev_periodic_enter_enabled = self.periodic_enter_enabled;

                    if self.periodic_enter_enabled {
                        ui.horizontal(|ui| {
                            ui.add_space(20.0);
                            ui.label("Interval:");
                            ui.add(
                                egui::Slider::new(&mut self.periodic_enter_interval, 10..=300)
                                    .suffix("s")
                            );
                        });

                        // Detect interval change: reset timer
                        if self.periodic_enter_interval != self.prev_periodic_enter_interval {
                            self.last_periodic_enter = Instant::now();
                        }
                        self.prev_periodic_enter_interval = self.periodic_enter_interval;

                        ui.horizontal(|ui| {
                            ui.add_space(20.0);
                            let remaining = self.periodic_enter_interval
                                .saturating_sub(self.last_periodic_enter.elapsed().as_secs());

                            // Show status based on armed/target state
                            if !self.armed {
                                ui.colored_label(egui::Color32::GRAY, format!("Next in: {}s (disarmed)", remaining));
                            } else if self.target_window.is_none() {
                                ui.colored_label(egui::Color32::GRAY, format!("Next in: {}s (no target)", remaining));
                            } else {
                                ui.colored_label(egui::Color32::GREEN, format!("Next in: {}s", remaining));
                            }
                        });
                    }
                });

            } // end Normal-mode sections (window + auto features)

            ui.separator();

            // Key preview
            ui.label("Key Preview:");
            egui::ScrollArea::vertical()
                .max_height(150.0)
                .show(ui, |ui| {
                    let preview_text: String = self
                        .key_preview
                        .iter()
                        .map(|k| k.display())
                        .collect();
                    ui.add(
                        egui::TextEdit::multiline(&mut preview_text.as_str())
                            .desired_width(f32::INFINITY)
                            .font(egui::TextStyle::Monospace),
                    );
                });

            if ui.button("Clear Preview").clicked() {
                self.key_preview.clear();
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_text_char_letters() {
        // Letters should be text chars
        assert!(DogkbdApp::is_text_char(&KeyPreview::Char('a')));
        assert!(DogkbdApp::is_text_char(&KeyPreview::Char('Z')));
    }

    #[test]
    fn test_is_text_char_digits() {
        // Digits should be text chars (they're KeyPreview::Char)
        assert!(DogkbdApp::is_text_char(&KeyPreview::Char('0')));
        assert!(DogkbdApp::is_text_char(&KeyPreview::Char('9')));
    }

    #[test]
    fn test_is_text_char_punctuation() {
        // Punctuation should be text chars (they're KeyPreview::Char)
        assert!(DogkbdApp::is_text_char(&KeyPreview::Char('!')));
        assert!(DogkbdApp::is_text_char(&KeyPreview::Char('.')));
        assert!(DogkbdApp::is_text_char(&KeyPreview::Char('-')));
    }

    #[test]
    fn test_is_text_char_space() {
        // Space should be a text char
        assert!(DogkbdApp::is_text_char(&KeyPreview::Space));
    }

    #[test]
    fn test_is_text_char_special_keys() {
        // Enter, AutoEnter, Backspace should NOT be text chars
        assert!(!DogkbdApp::is_text_char(&KeyPreview::Enter));
        assert!(!DogkbdApp::is_text_char(&KeyPreview::AutoEnter));
        assert!(!DogkbdApp::is_text_char(&KeyPreview::Backspace));
    }

    #[test]
    fn test_validation_char_threshold() {
        // Threshold should be 16
        assert_eq!(VALIDATION_CHAR_THRESHOLD, 16);
    }

    #[test]
    fn test_idle_timeout_secs() {
        // Idle timeout should be 5 seconds
        assert_eq!(IDLE_TIMEOUT_SECS, 5);
    }

    #[test]
    fn test_max_preview_keys() {
        // Max preview should be 50
        assert_eq!(MAX_PREVIEW_KEYS, 50);
    }

    #[test]
    fn test_collect_record_serializes_to_jsonl() {
        let line = serde_json::to_string(&CollectRecord {
            ts_ms: 1_700_000_000_000,
            seq: 7,
            text: "good dog",
        })
        .unwrap();
        assert_eq!(line, r#"{"ts_ms":1700000000000,"seq":7,"text":"good dog"}"#);
        // One record must serialize to a single line (no embedded newline).
        assert!(!line.contains('\n'));
    }

    #[test]
    fn test_collect_record_escapes_special_chars() {
        // Text with quotes/backslashes must stay valid JSON on one line.
        let line = serde_json::to_string(&CollectRecord {
            ts_ms: 0,
            seq: 1,
            text: "a\"b\\c",
        })
        .unwrap();
        assert!(line.contains(r#"\"b\\c"#));
        assert!(!line.contains('\n'));
    }

    #[test]
    fn test_count_existing_prompts() {
        let path = std::env::temp_dir()
            .join(format!("dogkbd_count_{}.jsonl", std::process::id()));
        let p = path.to_str().unwrap();

        // Missing file counts as 0.
        let _ = std::fs::remove_file(&path);
        assert_eq!(DogkbdApp::count_existing_prompts(p), 0);

        // Three records plus a trailing blank line → 3.
        std::fs::write(&path, "{\"a\":1}\n{\"a\":2}\n{\"a\":3}\n\n").unwrap();
        assert_eq!(DogkbdApp::count_existing_prompts(p), 3);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_session_path_format() {
        let p = default_session_path();
        // Per-session name, filesystem-safe (no colons on Windows).
        assert!(p.starts_with("dogkbd_prompts_"));
        assert!(p.ends_with(".jsonl"));
        assert!(!p.contains(':'));
    }

    #[test]
    fn test_collect_thresholds() {
        // Long prompts save faster than normal ones; min is below long.
        assert!(COLLECT_LONG_CHARS > COLLECT_MIN_CHARS);
        assert!(COLLECT_IDLE_SHORT_SECS < COLLECT_IDLE_LONG_SECS);
        assert_eq!(COLLECT_MIN_CHARS, 9);
        assert_eq!(COLLECT_LONG_CHARS, 16);
    }
}
