use chrono::Utc;
use eframe::egui;

use crate::alerts;
use crate::metric_registry;
use crate::settings::Settings;
use crate::types::{format_tokens, MetricsState};
use super::widgets;

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
            // Show last-updated age
            if let Some(last) = state.last_updated {
                let ago_secs = (now - last).num_seconds();
                let ago_label = if ago_secs < 60 {
                    format!("{}s ago", ago_secs)
                } else if ago_secs < 3600 {
                    format!("{}m ago", ago_secs / 60)
                } else {
                    format!("{}h ago", ago_secs / 3600)
                };
                ui.colored_label(
                    egui::Color32::from_rgb(120, 120, 120),
                    format!("Updated {}", ago_label),
                );
                ui.label(" | ");
            }
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
            ui.strong("API-Equiv Cost");
            let cost = state.estimated_cost(settings);
            ui.colored_label(alerts::cost_color(cost, settings), format!("${:.2}", cost));
        });
    });

    ui.separator();

    // Usage limit bars (per-model output tokens vs plan limits)
    {
        let window_usage = state.model_window_usage(settings.usage_window_hours);
        // Collect models that have limits configured, in fixed order
        let bar_models: Vec<(&str, &str, u64, egui::Color32)> = vec![
            ("opus", "Opus", settings.opus_output_limit, egui::Color32::from_rgb(180, 100, 255)),
            ("sonnet", "Sonnet", settings.sonnet_output_limit, egui::Color32::from_rgb(100, 180, 255)),
            ("haiku", "Haiku", settings.haiku_output_limit, egui::Color32::from_rgb(100, 220, 180)),
        ];

        // Only show if there's any usage in the window
        let has_usage = window_usage.values().any(|&v| v > 0);
        if has_usage {
            ui.label(format!(
                "Plan Usage ({:.0}h window, {})",
                settings.usage_window_hours,
                settings.plan_tier.label()
            ));
            ui.add_space(2.0);

            for (key, label, limit, base_color) in &bar_models {
                // Sum usage across all model variants matching this key
                let used: u64 = window_usage
                    .iter()
                    .filter(|(name, _)| name.contains(key))
                    .map(|(_, v)| *v)
                    .sum();
                if used == 0 {
                    continue;
                }
                let pct = if *limit > 0 {
                    (used as f64 / *limit as f64).min(1.0)
                } else {
                    0.0
                };
                let pct_display = (pct * 100.0) as u32;

                // Color based on usage level
                let bar_color = if pct < 0.60 {
                    egui::Color32::from_rgb(0, 200, 80) // green
                } else if pct < 0.85 {
                    egui::Color32::from_rgb(255, 200, 50) // yellow
                } else {
                    egui::Color32::from_rgb(255, 80, 60) // red
                };

                ui.horizontal(|ui| {
                    ui.colored_label(*base_color, format!("{:6}", label));
                    let available_width = ui.available_width() - 120.0;
                    let bar_width = available_width.max(60.0);
                    let (rect, _) = ui.allocate_exact_size(
                        egui::vec2(bar_width, 14.0),
                        egui::Sense::hover(),
                    );
                    // Background
                    ui.painter().rect_filled(
                        rect,
                        2.0,
                        egui::Color32::from_rgb(40, 40, 40),
                    );
                    // Filled portion
                    let filled_rect = egui::Rect::from_min_size(
                        rect.min,
                        egui::vec2(rect.width() * pct as f32, rect.height()),
                    );
                    ui.painter().rect_filled(filled_rect, 2.0, bar_color);

                    ui.label(format!(
                        "{}% ({})",
                        pct_display,
                        crate::types::format_tokens(used)
                    ));
                });
            }
            ui.separator();
        }
    }

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
            if let Some(def) = metric_registry::lookup("burn_rate") {
                widgets::metric_class_indicator(ui, def);
            }
        });
        ui.separator();
    }
}

/// Extract a short display name from a full model identifier.
/// "claude-opus-4-6" → "opus-4-6", "claude-sonnet-4-5" → "sonnet-4-5"
fn friendly_display_name(model: &str) -> &str {
    model.strip_prefix("claude-").unwrap_or(model)
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
