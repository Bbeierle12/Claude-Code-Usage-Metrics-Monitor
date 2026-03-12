use eframe::egui;

use crate::settings::{Settings, ValidationErrors};

/// Modal for editing runtime settings.
pub struct SettingsModal {
    pub draft: Settings,
    pub errors: ValidationErrors,
    pub save_error: Option<String>,
    want_save: bool,
    want_cancel: bool,
}

impl SettingsModal {
    pub fn new(current: &Settings) -> Self {
        Self {
            draft: current.clone(),
            errors: ValidationErrors::default(),
            save_error: None,
            want_save: false,
            want_cancel: false,
        }
    }

    /// Render the modal. Returns `true` to stay open, `false` to close.
    pub fn render(&mut self, ctx: &egui::Context, live_settings: &mut Settings) -> bool {
        // Reset click flags each frame
        self.want_save = false;
        self.want_cancel = false;

        let mut x_closed = true; // tracks the X button via .open()

        egui::Window::new("Settings")
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .collapsible(false)
            .resizable(false)
            .fixed_size([440.0, 620.0])
            .open(&mut x_closed)
            .show(ctx, |ui| {
                self.render_contents(ui);
            });

        // Handle save outside the closure
        if self.want_save && self.errors.is_empty() {
            match self.draft.save() {
                Ok(()) => {
                    *live_settings = self.draft.clone();
                    return false; // close
                }
                Err(e) => {
                    self.save_error = Some(e);
                }
            }
        }

        if self.want_cancel || !x_closed {
            return false; // close
        }

        true
    }

    fn render_contents(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical().show(ui, |ui| {
            // ── Alert Thresholds ──
            ui.strong("Alert Thresholds");
            ui.add_space(2.0);
            egui::Grid::new("alert_grid")
                .num_columns(2)
                .spacing([8.0, 4.0])
                .show(ui, |ui| {
                    ui.label("Warning ($):");
                    ui.add(
                        egui::DragValue::new(&mut self.draft.daily_cost_warn)
                            .range(0.0..=1000.0)
                            .speed(0.5),
                    );
                    ui.end_row();

                    ui.label("Critical ($):");
                    ui.add(
                        egui::DragValue::new(&mut self.draft.daily_cost_critical)
                            .range(0.0..=1000.0)
                            .speed(0.5),
                    );
                    ui.end_row();
                });
            ui.add_space(8.0);

            // ── Burn Rate ──
            ui.strong("Burn Rate");
            ui.add_space(2.0);
            egui::Grid::new("burn_grid")
                .num_columns(2)
                .spacing([8.0, 4.0])
                .show(ui, |ui| {
                    ui.label("Window (min):");
                    ui.add(
                        egui::DragValue::new(&mut self.draft.burn_rate_window_minutes)
                            .range(1..=120)
                            .speed(1),
                    );
                    ui.end_row();

                    ui.label("Low (tok/min):");
                    ui.add(
                        egui::DragValue::new(&mut self.draft.burn_rate_low)
                            .range(0.0..=100_000.0)
                            .speed(10.0),
                    );
                    ui.end_row();

                    ui.label("High (tok/min):");
                    ui.add(
                        egui::DragValue::new(&mut self.draft.burn_rate_high)
                            .range(0.0..=100_000.0)
                            .speed(10.0),
                    );
                    ui.end_row();
                });
            ui.add_space(8.0);

            // ── Session ──
            ui.strong("Session");
            ui.add_space(2.0);
            egui::Grid::new("session_grid")
                .num_columns(2)
                .spacing([8.0, 4.0])
                .show(ui, |ui| {
                    ui.label("Active threshold (min):");
                    ui.add(
                        egui::DragValue::new(&mut self.draft.active_session_threshold_minutes)
                            .range(1..=60)
                            .speed(1),
                    );
                    ui.end_row();
                });
            ui.add_space(8.0);

            // ── Model Pricing ──
            ui.strong("Model Pricing ($/M tokens)");
            ui.add_space(2.0);
            Self::pricing_grid(ui, "opus_pricing", &mut self.draft.opus_pricing, "Opus");
            Self::pricing_grid(
                ui,
                "sonnet_pricing",
                &mut self.draft.sonnet_pricing,
                "Sonnet",
            );
            Self::pricing_grid(
                ui,
                "haiku_pricing",
                &mut self.draft.haiku_pricing,
                "Haiku",
            );
            ui.add_space(8.0);

            // ── Plan Limits ──
            ui.strong("Plan Limits");
            ui.add_space(2.0);
            egui::Grid::new("plan_grid")
                .num_columns(2)
                .spacing([8.0, 4.0])
                .show(ui, |ui| {
                    ui.label("Plan tier:");
                    egui::ComboBox::from_id_salt("plan_tier")
                        .selected_text(self.draft.plan_tier.label())
                        .show_ui(ui, |ui| {
                            let prev_tier = self.draft.plan_tier;
                            for tier in crate::settings::PlanTier::ALL {
                                ui.selectable_value(
                                    &mut self.draft.plan_tier,
                                    tier,
                                    tier.label(),
                                );
                            }
                            if self.draft.plan_tier != prev_tier {
                                self.draft.apply_tier_defaults();
                            }
                        });
                    ui.end_row();

                    ui.label("Window (hours):");
                    ui.add(
                        egui::DragValue::new(&mut self.draft.usage_window_hours)
                            .range(0.5..=24.0)
                            .speed(0.5),
                    );
                    ui.end_row();

                    ui.label("Opus limit:");
                    ui.add(
                        egui::DragValue::new(&mut self.draft.opus_output_limit)
                            .range(0..=100_000_000u64)
                            .speed(10000.0),
                    );
                    ui.end_row();

                    ui.label("Sonnet limit:");
                    ui.add(
                        egui::DragValue::new(&mut self.draft.sonnet_output_limit)
                            .range(0..=100_000_000u64)
                            .speed(10000.0),
                    );
                    ui.end_row();

                    ui.label("Haiku limit:");
                    ui.add(
                        egui::DragValue::new(&mut self.draft.haiku_output_limit)
                            .range(0..=100_000_000u64)
                            .speed(10000.0),
                    );
                    ui.end_row();
                });
            ui.add_space(8.0);

            // ── Paths (read-only) ──
            ui.strong("Paths");
            ui.add_space(2.0);
            let home = dirs::home_dir().unwrap_or_default();
            ui.label(format!(
                "Projects: {}",
                home.join(crate::config::CLAUDE_PROJECTS_REL).display()
            ));
            ui.label(format!(
                "Database: {}",
                crate::storage::db_path().display()
            ));
            ui.label(format!("Settings: {}", Settings::path().display()));
            ui.add_space(8.0);

            // ── Validation errors ──
            self.errors = self.draft.validate();
            for err in &self.errors.errors {
                ui.colored_label(egui::Color32::from_rgb(255, 80, 60), err);
            }
            if let Some(ref e) = self.save_error {
                ui.colored_label(
                    egui::Color32::from_rgb(255, 80, 60),
                    format!("Save failed: {}", e),
                );
            }
            ui.add_space(4.0);

            // ── Buttons ──
            ui.horizontal(|ui| {
                if ui.button("Reset Defaults").clicked() {
                    self.draft = Settings::default();
                    self.save_error = None;
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let save_enabled = self.errors.is_empty();
                    if ui
                        .add_enabled(save_enabled, egui::Button::new("Save"))
                        .clicked()
                    {
                        self.want_save = true;
                    }
                    if ui.button("Cancel").clicked() {
                        self.want_cancel = true;
                    }
                });
            });
        });
    }

    fn pricing_grid(
        ui: &mut egui::Ui,
        id: &str,
        pricing: &mut crate::settings::ModelPricing,
        label: &str,
    ) {
        ui.label(format!("  {}", label));
        egui::Grid::new(id)
            .num_columns(4)
            .spacing([6.0, 2.0])
            .show(ui, |ui| {
                ui.label("In:");
                ui.add(
                    egui::DragValue::new(&mut pricing.input_per_m)
                        .range(0.0..=500.0)
                        .speed(0.1),
                );
                ui.label("Out:");
                ui.add(
                    egui::DragValue::new(&mut pricing.output_per_m)
                        .range(0.0..=500.0)
                        .speed(0.1),
                );
                ui.end_row();

                ui.label("CW:");
                ui.add(
                    egui::DragValue::new(&mut pricing.cache_write_per_m)
                        .range(0.0..=500.0)
                        .speed(0.1),
                );
                ui.label("CR:");
                ui.add(
                    egui::DragValue::new(&mut pricing.cache_read_per_m)
                        .range(0.0..=500.0)
                        .speed(0.01),
                );
                ui.end_row();
            });
        ui.add_space(4.0);
    }
}
