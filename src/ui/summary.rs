use chrono::Utc;
use eframe::egui;

use crate::alerts;
use crate::settings::Settings;
use crate::types::{format_tokens, MetricsState};

pub fn render(ui: &mut egui::Ui, state: &MetricsState, settings: &Settings) {
    let now = Utc::now();
    let active = state.active_session_count(settings);

    // Header
    ui.horizontal(|ui| {
        if active > 0 {
            ui.colored_label(egui::Color32::from_rgb(0, 200, 80), "\u{25CF}"); // green dot
            ui.colored_label(
                egui::Color32::from_rgb(0, 200, 80),
                format!("{} active", active),
            );
        } else {
            ui.colored_label(egui::Color32::from_rgb(120, 120, 120), "\u{25CF}"); // gray dot
            ui.colored_label(egui::Color32::from_rgb(120, 120, 120), "idle");
        }

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(now.format("%Y-%m-%d").to_string());
        });
    });

    ui.separator();

    // Token totals
    ui.columns(4, |cols| {
        cols[0].vertical_centered(|ui| {
            ui.strong("Input");
            ui.label(format_tokens(state.total_input));
        });
        cols[1].vertical_centered(|ui| {
            ui.strong("Output");
            ui.label(format_tokens(state.total_output));
        });
        cols[2].vertical_centered(|ui| {
            ui.strong("Cache R/W");
            ui.label(format!(
                "{} / {}",
                format_tokens(state.total_cache_read),
                format_tokens(state.total_cache_creation)
            ));
        });
        cols[3].vertical_centered(|ui| {
            ui.strong("Est. Cost");
            let cost = state.estimated_cost(settings);
            ui.colored_label(alerts::cost_color(cost, settings), format!("${:.2}", cost));
        });
    });

    ui.separator();

    // Model breakdown (compact inline, sorted for deterministic order)
    if !state.models.is_empty() {
        ui.horizontal_wrapped(|ui| {
            ui.label("Models:");
            let mut models: Vec<_> = state.models.iter().collect();
            models.sort_by_key(|(name, _)| (*name).clone());
            let model_count = models.len();
            for (i, (name, m)) in models.iter().enumerate() {
                let cost = settings.estimate_cost(
                    name,
                    m.input_tokens,
                    m.output_tokens,
                    m.cache_creation_tokens,
                    m.cache_read_tokens,
                );
                let display_name = friendly_display_name(name);
                let color = model_color(&display_name);
                ui.colored_label(
                    color,
                    format!(
                        "{}: {} msgs, ${:.2}",
                        capitalize(&display_name),
                        m.message_count,
                        cost
                    ),
                );
                if i < model_count - 1 {
                    ui.label(" | ");
                }
            }
        });
        ui.separator();
    }

    // Burn rate indicator
    let burn_rate = state.burn_rate_per_minute(settings);
    if burn_rate > 0.0 {
        ui.horizontal(|ui| {
            ui.label("Burn Rate:");
            let color = if burn_rate < settings.burn_rate_low {
                egui::Color32::from_rgb(0, 200, 80) // green
            } else if burn_rate < settings.burn_rate_high {
                egui::Color32::from_rgb(255, 200, 50) // yellow
            } else {
                egui::Color32::from_rgb(255, 80, 60) // red
            };
            let label = if burn_rate >= 1000.0 {
                format!("{:.1}K tok/min", burn_rate / 1000.0)
            } else {
                format!("{:.0} tok/min", burn_rate)
            };
            ui.colored_label(color, label);
        });
        ui.separator();
    }
}

/// Extract a short display name from a full model identifier.
/// "claude-opus-4-6" → "opus-4-6", "claude-sonnet-4-5" → "sonnet-4-5"
fn friendly_display_name(model: &str) -> String {
    model
        .strip_prefix("claude-")
        .unwrap_or(model)
        .to_string()
}

fn model_color(name: &str) -> egui::Color32 {
    if name.contains("opus") {
        egui::Color32::from_rgb(180, 100, 255)  // purple
    } else if name.contains("sonnet") {
        egui::Color32::from_rgb(100, 180, 255) // blue
    } else if name.contains("haiku") {
        egui::Color32::from_rgb(100, 220, 180)  // teal
    } else {
        egui::Color32::from_rgb(180, 180, 180)
    }
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        None => String::new(),
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
    }
}
