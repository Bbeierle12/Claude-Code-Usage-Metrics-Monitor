use chrono::Utc;
use eframe::egui;

use crate::config;
use crate::types::MetricsState;

pub fn render(ui: &mut egui::Ui, state: &MetricsState) {
    let now = Utc::now();
    let active = state.active_session_count();

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
            ui.colored_label(
                egui::Color32::from_rgb(255, 180, 50),
                format!("${:.2}", state.estimated_cost()),
            );
        });
    });

    ui.separator();

    // Model breakdown (compact inline)
    if !state.models.is_empty() {
        ui.horizontal_wrapped(|ui| {
            ui.label("Models:");
            for (name, m) in &state.models {
                let cost = config::estimate_cost(
                    name,
                    m.input_tokens,
                    m.output_tokens,
                    m.cache_creation_tokens,
                    m.cache_read_tokens,
                );
                let color = model_color(name);
                ui.colored_label(
                    color,
                    format!(
                        "{}: {} msgs, ${:.2}",
                        capitalize(name),
                        m.message_count,
                        cost
                    ),
                );
                ui.label(" | ");
            }
        });
        ui.separator();
    }
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

fn model_color(name: &str) -> egui::Color32 {
    match name {
        "opus" => egui::Color32::from_rgb(180, 100, 255),  // purple
        "sonnet" => egui::Color32::from_rgb(100, 180, 255), // blue
        "haiku" => egui::Color32::from_rgb(100, 220, 180),  // teal
        _ => egui::Color32::from_rgb(180, 180, 180),
    }
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        None => String::new(),
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
    }
}
