//! DOGKBD Receiver GUI application

use crate::keys::KeyPreview;
use crate::target::WindowInfo;
use dogkbd_proto::KeyTap;
use eframe::egui;
use rodio::{Decoder, OutputStream, Sink};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Maximum number of key previews to show
const MAX_PREVIEW_KEYS: usize = 50;

/// Idle timeout for auto-enter and sequence reset (6 seconds)
const IDLE_TIMEOUT_SECS: u64 = 6;

/// Minimum total text characters required to submit (auto-enter) and dispense treat
const VALIDATION_CHAR_THRESHOLD: usize = 10;

/// Minimum text characters typed while Claude is idle
const IDLE_CHAR_THRESHOLD: usize = 4;

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

            // Feature 1: Auto-enter on idle (enabled by default)
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
            audio_stream,
            audio_sink,
            audio_available,

            // Feature 3: Periodic auto-enter (disabled by default)
            periodic_enter_enabled: false,
            prev_periodic_enter_enabled: false,
            periodic_enter_interval: 60,
            prev_periodic_enter_interval: 60,
            last_periodic_enter: Instant::now(),
            enter_injected_this_frame: false,

            // Input delay (0ms by default)
            input_delay_ms: 0,
            delay_buffer: VecDeque::new(),

            // Claude Code busy state (idle by default)
            claude_busy,
            claude_was_busy: false,
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

        // Track input timing
        self.last_input_time = Some(Instant::now());

        // Add to preview
        if let Some(preview) = KeyPreview::from_tap(tap) {
            self.has_input_since_enter = true;

            // Track text characters for treat threshold
            let claude_idle = !self.claude_busy.load(Ordering::Relaxed);
            // Backspace decrements count (but not below 0)
            if matches!(preview, KeyPreview::Backspace) {
                self.text_char_count = self.text_char_count.saturating_sub(1);
                if claude_idle {
                    self.idle_char_count = self.idle_char_count.saturating_sub(1);
                }
                // If count dropped below threshold, allow treat to dispense again
                if self.text_char_count < VALIDATION_CHAR_THRESHOLD
                    || self.idle_char_count < IDLE_CHAR_THRESHOLD
                {
                    self.validation_tone_played = false;
                }
            } else if Self::is_text_char(&preview) {
                self.text_char_count += 1;
                if claude_idle {
                    self.idle_char_count += 1;
                }

                // Dispense treat when both thresholds reached (not yet dispensed this sequence)
                // Only dispense when armed AND Claude is idle
                if self.text_char_count >= VALIDATION_CHAR_THRESHOLD
                    && self.idle_char_count >= IDLE_CHAR_THRESHOLD
                    && !self.validation_tone_played
                    && self.armed
                    && claude_idle
                {
                    Self::dispense_treat();
                    self.validation_tone_played = true;
                }
            }

            self.key_preview.push(preview);
            if self.key_preview.len() > MAX_PREVIEW_KEYS {
                self.key_preview.remove(0);
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
        // Don't auto-enter while Claude Code is processing
        if self.claude_busy.load(Ordering::Relaxed) {
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
                    if self.inject_enter() {
                        self.status = "Auto-enter injected (idle timeout)".to_string();
                        // Defer chime to busy→idle transition
                        if self.validation_tone_enabled {
                            self.chime_pending = true;
                        }
                    }
                    // Reset sequence counters after submission
                    self.text_char_count = 0;
                    self.idle_char_count = 0;
                    self.validation_tone_played = false;
                }

                // Clear last_input_time to prevent repeated triggers
                self.last_input_time = None;
            }
        }
    }

    /// Check and handle periodic auto-enter
    fn check_periodic_enter(&mut self) {
        // Don't periodic-enter while Claude Code is processing
        if self.claude_busy.load(Ordering::Relaxed) {
            return;
        }

        if self.periodic_enter_enabled {
            let elapsed = self.last_periodic_enter.elapsed();
            if elapsed >= Duration::from_secs(self.periodic_enter_interval) {
                // Only inject if there's actual input to submit
                if self.has_input_since_enter {
                    if self.inject_enter() {
                        self.status = "Periodic auto-enter injected".to_string();
                        // Defer chime to busy→idle transition
                        if self.validation_tone_enabled {
                            self.chime_pending = true;
                        }
                    }
                    // Reset sequence counters after submission
                    self.text_char_count = 0;
                    self.idle_char_count = 0;
                    self.validation_tone_played = false;
                }
                self.last_periodic_enter = Instant::now();
            }
        }
    }
}

impl eframe::App for DogkbdApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Reset per-frame flags
        self.enter_injected_this_frame = false;

        // Detect state transitions
        let claude_busy_now = self.claude_busy.load(Ordering::Relaxed);
        if !claude_busy_now && self.claude_was_busy {
            // busy → idle transition
            // Reset periodic timer to prevent accumulated time from firing immediately
            self.last_periodic_enter = Instant::now();
            // Play deferred chime now that Claude is done
            if self.chime_pending {
                self.play_validation_tone();
                self.chime_pending = false;
            }
        } else if claude_busy_now && !self.claude_was_busy {
            // idle → busy transition: only chars typed during current idle period count
            self.idle_char_count = 0;
        }
        self.claude_was_busy = claude_busy_now;

        // Process any pending keys
        self.process_keys();

        // Check timer-based features
        self.check_idle_timeout();
        self.check_periodic_enter();

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
                if self.claude_busy.load(Ordering::Relaxed) {
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

            // Arm toggle
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
                    self.text_char_count = 0;
                    self.idle_char_count = 0;
                    self.validation_tone_played = false;
                    self.chime_pending = false;
                    self.has_input_since_enter = false;
                    self.last_input_time = None;
                    self.last_periodic_enter = Instant::now();
                    self.key_preview.clear();
                    self.status = "State reset".to_string();
                }
            });

            ui.separator();

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
                    ui.checkbox(&mut self.auto_enter_on_idle, "Auto-enter on idle (6s)")
                        .on_hover_text("Inject Enter after 6 seconds of no input (requires 10+ text characters)");

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
                        if self.validation_tone_played {
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
        // Threshold should be 10
        assert_eq!(VALIDATION_CHAR_THRESHOLD, 10);
    }

    #[test]
    fn test_idle_timeout_secs() {
        // Idle timeout should be 6 seconds
        assert_eq!(IDLE_TIMEOUT_SECS, 6);
    }

    #[test]
    fn test_max_preview_keys() {
        // Max preview should be 50
        assert_eq!(MAX_PREVIEW_KEYS, 50);
    }
}
