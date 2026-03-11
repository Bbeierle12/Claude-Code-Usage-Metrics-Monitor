use eframe::egui::{self, Color32};

use crate::metric_registry;
use crate::settings::Settings;
use crate::types::MetricsState;
use super::widgets;

pub fn render(ui: &mut egui::Ui, state: &MetricsState, settings: &Settings) {
    ui.strong("Cost Intelligence");
    ui.add_space(4.0);

    // Cache efficiency
    let cache_eff = if !state.cost_intel.cache_efficiency_samples.is_empty() {
        let sum: f64 = state.cost_intel.cache_efficiency_samples.iter().map(|(_, e)| e).sum();
        sum / state.cost_intel.cache_efficiency_samples.len() as f64
    } else {
        0.0
    };
    let cache_color = if cache_eff > 0.7 {
        Color32::from_rgb(100, 200, 100)
    } else if cache_eff > 0.4 {
        Color32::from_rgb(200, 200, 100)
    } else {
        Color32::from_rgb(200, 100, 100)
    };
    ui.horizontal(|ui| {
        widgets::render_gauge(ui, "Cache efficiency", cache_eff, cache_color, 120.0);
        if let Some(def) = metric_registry::lookup("cache_efficiency") {
            widgets::metric_class_indicator(ui, def);
        }
    });

    // Cache efficiency trend sparkline
    if state.cost_intel.cache_efficiency_samples.len() > 5 {
        let samples: Vec<f64> = state.cost_intel.cache_efficiency_samples
            .iter()
            .map(|(_, e)| *e)
            .collect();
        let height = 30.0;
        let width = ui.available_width().min(300.0);
        let (rect, _) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 2.0, Color32::from_rgb(30, 30, 30));

        if samples.len() >= 2 {
            let max_val = samples.iter().cloned().fold(0.01_f64, f64::max);
            let points: Vec<egui::Pos2> = samples.iter().enumerate().map(|(i, v)| {
                let x = rect.min.x + (i as f32 / (samples.len() - 1) as f32) * rect.width();
                let y = rect.max.y - (*v / max_val) as f32 * rect.height();
                egui::pos2(x, y)
            }).collect();
            for pair in points.windows(2) {
                painter.line_segment([pair[0], pair[1]], egui::Stroke::new(1.5, cache_color));
            }
        }
    }

    ui.add_space(8.0);

    // Cost per tool
    if !state.cost_intel.cost_per_tool.is_empty() {
        ui.label("Cost per tool:");
        ui.add_space(2.0);

        let mut sorted: Vec<_> = state.cost_intel.cost_per_tool.iter().collect();
        sorted.sort_by(|a, b| b.1.partial_cmp(a.1).unwrap_or(std::cmp::Ordering::Equal));

        let max_cost = sorted.first().map(|(_, c)| **c).unwrap_or(1.0).max(0.01);

        egui::ScrollArea::vertical()
            .id_salt("cost_per_tool")
            .max_height(150.0)
            .show(ui, |ui| {
                for (tool, cost) in sorted.iter().take(15) {
                    ui.horizontal(|ui| {
                        ui.label(format!("{:>12}", tool));
                        let bar_frac = (**cost / max_cost) as f32;
                        let (rect, _) = ui.allocate_exact_size(
                            egui::vec2(150.0, 12.0),
                            egui::Sense::hover(),
                        );
                        let painter = ui.painter_at(rect);
                        painter.rect_filled(rect, 2.0, Color32::from_rgb(40, 40, 40));
                        let bar = egui::Rect::from_min_size(
                            rect.min,
                            egui::vec2(rect.width() * bar_frac, rect.height()),
                        );
                        painter.rect_filled(bar, 2.0, Color32::from_rgb(100, 180, 255));
                        ui.label(format!("${:.3}", cost));
                    });
                }
            });
    }

    ui.add_space(8.0);

    // Token waste
    let waste_color = if state.cost_intel.token_waste_events > 3 {
        Color32::from_rgb(255, 100, 100)
    } else {
        Color32::from_rgb(180, 180, 180)
    };
    ui.horizontal(|ui| {
        widgets::render_metric_row(ui, "Token waste events", &state.cost_intel.token_waste_events.to_string(), waste_color);
        if let Some(def) = metric_registry::lookup("token_waste") {
            widgets::metric_class_indicator(ui, def);
        }
    });
    if state.cost_intel.token_waste_tokens > 0 {
        widgets::render_metric_row(
            ui,
            "  Wasted tokens",
            &crate::types::format_tokens(state.cost_intel.token_waste_tokens),
            waste_color,
        );
    }

    // Cost efficiency score (composite)
    let total_cost = state.estimated_cost(settings);
    let total_output = state.total_output;
    let cost_per_1k_output = if total_output > 0 {
        total_cost / (total_output as f64 / 1000.0)
    } else {
        0.0
    };
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        widgets::render_metric_row(ui, "Cost per 1K output", &format!("${:.4}", cost_per_1k_output), Color32::from_rgb(180, 180, 180));
        if let Some(def) = metric_registry::lookup("cost_per_1k_output") {
            widgets::metric_class_indicator(ui, def);
        }
    });
}
