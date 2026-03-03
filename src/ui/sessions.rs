use chrono::Utc;
use eframe::egui;

use crate::config;
use crate::types::MetricsState;

pub fn render(ui: &mut egui::Ui, state: &MetricsState) {
    let sessions = state.sessions_sorted();
    let now = Utc::now();

    if sessions.is_empty() {
        ui.colored_label(
            egui::Color32::from_rgb(120, 120, 120),
            "No sessions today",
        );
        return;
    }

    ui.strong(format!("Sessions ({})", sessions.len()));
    ui.add_space(4.0);

    egui::ScrollArea::vertical()
        .max_height(200.0)
        .show(ui, |ui| {
            egui::Grid::new("sessions_grid")
                .num_columns(5)
                .spacing([12.0, 4.0])
                .striped(true)
                .show(ui, |ui| {
                    // Header
                    ui.strong("Project");
                    ui.strong("Model");
                    ui.strong("Duration");
                    ui.strong("Tokens");
                    ui.strong("Cost");
                    ui.end_row();

                    for s in &sessions {
                        let is_active =
                            s.is_active(now, config::ACTIVE_SESSION_THRESHOLD_MINUTES);
                        let text_color = if is_active {
                            egui::Color32::from_rgb(200, 230, 255)
                        } else {
                            egui::Color32::from_rgb(180, 180, 180)
                        };

                        // Project name (truncated)
                        let proj = if s.project.len() > 20 {
                            format!("{}...", &s.project[..17])
                        } else {
                            s.project.clone()
                        };
                        ui.colored_label(text_color, &proj);

                        // Model
                        let model_short = if s.model.contains("opus") {
                            "Opus"
                        } else if s.model.contains("sonnet") {
                            "Sonnet"
                        } else if s.model.contains("haiku") {
                            "Haiku"
                        } else {
                            &s.model
                        };
                        ui.colored_label(text_color, model_short);

                        // Duration
                        let mins = s.duration_minutes();
                        let dur = if mins >= 60 {
                            format!("{}h{}m", mins / 60, mins % 60)
                        } else {
                            format!("{}m", mins)
                        };
                        ui.colored_label(text_color, dur);

                        // Tokens
                        ui.colored_label(text_color, format_tokens(s.total_tokens()));

                        // Cost
                        let cost = config::estimate_cost(
                            &s.model,
                            s.input_tokens,
                            s.output_tokens,
                            s.cache_creation_tokens,
                            s.cache_read_tokens,
                        );
                        ui.colored_label(text_color, format!("${:.2}", cost));

                        ui.end_row();
                    }
                });
        });
}

fn format_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}
