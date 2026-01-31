//! DOGKBD Receiver GUI application

use crate::keys::KeyPreview;
use crate::target::WindowInfo;
use dogkbd_proto::KeyTap;
use eframe::egui;
use rodio::{OutputStream, Sink, Source};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

/// Maximum number of key previews to show
const MAX_PREVIEW_KEYS: usize = 50;

/// Idle timeout for auto-enter and sequence reset (30 seconds)
const IDLE_TIMEOUT_SECS: u64 = 30;

/// Number of text characters required to trigger validation tone
const VALIDATION_CHAR_THRESHOLD: usize = 8;

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
    /// Count of text characters in current input sequence
    text_char_count: usize,
    /// Whether the validation tone has been played for this sequence
    validation_tone_played: bool,
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
}

impl DogkbdApp {
    pub fn new(rx: Receiver<KeyTap>) -> Self {
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
            validation_tone_played: false,
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

    /// Play the validation tone
    fn play_validation_tone(&self) {
        if let Some(ref sink) = self.audio_sink {
            // Generate a pleasant beep tone (800Hz for 150ms)
            let source = rodio::source::SineWave::new(800.0)
                .take_duration(Duration::from_millis(150))
                .amplify(0.3);
            sink.append(source);
        }
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

    /// Process pending key taps
    fn process_keys(&mut self) {
        while let Ok(tap) = self.rx.try_recv() {
            self.packet_count += 1;

            // Track input timing
            self.last_input_time = Some(Instant::now());

            // Add to preview
            if let Some(preview) = KeyPreview::from_tap(&tap) {
                // Track for auto-enter on idle feature
                if matches!(preview, KeyPreview::Enter) {
                    self.has_input_since_enter = false;
                    // Also reset validation tone sequence on Enter
                    self.text_char_count = 0;
                    self.validation_tone_played = false;
                } else {
                    self.has_input_since_enter = true;
                }

                // Track text characters for validation tone
                // Backspace decrements count (but not below 0)
                if matches!(preview, KeyPreview::Backspace) {
                    self.text_char_count = self.text_char_count.saturating_sub(1);
                    // If count dropped below threshold, allow tone to play again
                    if self.text_char_count < VALIDATION_CHAR_THRESHOLD {
                        self.validation_tone_played = false;
                    }
                } else if Self::is_text_char(&preview) {
                    self.text_char_count += 1;

                    // Play validation tone if threshold reached and not yet played
                    if self.validation_tone_enabled
                        && self.text_char_count >= VALIDATION_CHAR_THRESHOLD
                        && !self.validation_tone_played
                    {
                        self.play_validation_tone();
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
                        match crate::inject::inject(&tap) {
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
    }

    /// Check and handle idle timeout (auto-enter and sequence reset)
    fn check_idle_timeout(&mut self) {
        if let Some(last_input) = self.last_input_time {
            let idle_duration = last_input.elapsed();

            if idle_duration >= Duration::from_secs(IDLE_TIMEOUT_SECS) {
                // Reset validation tone sequence
                if self.text_char_count > 0 || self.validation_tone_played {
                    self.text_char_count = 0;
                    self.validation_tone_played = false;
                }

                // Auto-enter on idle if enabled and we have input since last enter
                if self.auto_enter_on_idle && self.has_input_since_enter {
                    if self.inject_enter() {
                        // inject_enter already resets has_input_since_enter
                        self.status = "Auto-enter injected (idle timeout)".to_string();
                    }
                }

                // Clear last_input_time to prevent repeated triggers
                self.last_input_time = None;
            }
        }
    }

    /// Check and handle periodic auto-enter
    fn check_periodic_enter(&mut self) {
        if self.periodic_enter_enabled {
            let elapsed = self.last_periodic_enter.elapsed();
            if elapsed >= Duration::from_secs(self.periodic_enter_interval) {
                if self.inject_enter() {
                    self.status = "Periodic auto-enter injected".to_string();
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
                    // Feature 1: Auto-enter on idle
                    ui.checkbox(&mut self.auto_enter_on_idle, "Auto-enter on idle (30s)")
                        .on_hover_text("Inject Enter after 30 seconds of no input (only if there was input since last Enter)");

                    ui.add_space(4.0);

                    // Feature 2: Validation tone
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut self.validation_tone_enabled, "Validation tone")
                            .on_hover_text("Play a tone after 8 text characters are received to confirm input acceptance");
                        if !self.audio_available {
                            ui.colored_label(egui::Color32::YELLOW, "(audio unavailable)");
                        } else if ui.small_button("Test").clicked() {
                            self.play_validation_tone();
                        }
                    });

                    // Detect toggle: reset state when re-enabled
                    if self.validation_tone_enabled && !self.prev_validation_tone_enabled {
                        self.text_char_count = 0;
                        self.validation_tone_played = false;
                    }
                    self.prev_validation_tone_enabled = self.validation_tone_enabled;

                    if self.validation_tone_enabled {
                        ui.horizontal(|ui| {
                            ui.add_space(20.0);
                            ui.label(format!("Chars: {}/{}", self.text_char_count, VALIDATION_CHAR_THRESHOLD));
                            if self.validation_tone_played {
                                ui.colored_label(egui::Color32::GREEN, "✓ Tone played");
                            }
                        });
                    }

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
        // Threshold should be 8
        assert_eq!(VALIDATION_CHAR_THRESHOLD, 8);
    }

    #[test]
    fn test_idle_timeout_secs() {
        // Idle timeout should be 30 seconds
        assert_eq!(IDLE_TIMEOUT_SECS, 30);
    }

    #[test]
    fn test_max_preview_keys() {
        // Max preview should be 50
        assert_eq!(MAX_PREVIEW_KEYS, 50);
    }
}
