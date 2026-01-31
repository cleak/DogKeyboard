//! DOGKBD Receiver GUI application

use crate::keys::KeyPreview;
use crate::target::WindowInfo;
use dogkbd_proto::KeyTap;
use eframe::egui;
use std::sync::mpsc::Receiver;

/// Maximum number of key previews to show
const MAX_PREVIEW_KEYS: usize = 50;

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
}

impl DogkbdApp {
    pub fn new(rx: Receiver<KeyTap>) -> Self {
        let mut app = Self {
            rx,
            armed: false,
            target_window: None,
            available_windows: Vec::new(),
            key_preview: Vec::new(),
            status: "Ready".to_string(),
            error: None,
            packet_count: 0,
            inject_count: 0,
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

    /// Process pending key taps
    fn process_keys(&mut self) {
        while let Ok(tap) = self.rx.try_recv() {
            self.packet_count += 1;

            // Add to preview
            if let Some(preview) = KeyPreview::from_tap(&tap) {
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
}

impl eframe::App for DogkbdApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Process any pending keys
        self.process_keys();

        // Request repaint to check for new keys
        ctx.request_repaint_after(std::time::Duration::from_millis(16));

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

            // Key preview
            ui.label("Key Preview:");
            egui::ScrollArea::vertical()
                .max_height(200.0)
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
