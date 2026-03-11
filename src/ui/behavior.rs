use eframe::egui::{self, Color32};

use crate::metric_registry;
use crate::types::MetricsState;
use super::widgets;

pub fn render(ui: &mut egui::Ui, state: &MetricsState) {
    ui.strong("Behavioral Analytics");
    ui.add_space(4.0);

    if state.session_behaviors.is_empty() {
        ui.colored_label(Color32::from_rgb(120, 120, 120), "No behavioral data yet");
        return;
    }

    // Aggregate across all sessions
    let mut total_search: u64 = 0;
    let mut total_action: u64 = 0;
    let mut total_old_len: u64 = 0;
    let mut total_new_len: u64 = 0;
    let mut total_retries: u64 = 0;
    let mut total_tdd_cycles: u64 = 0;
    let mut total_breadth: usize = 0;
    let mut total_edit_ops: u64 = 0;

    for behavior in state.session_behaviors.values() {
        total_search += behavior.search_ops;
        total_action += behavior.action_ops;
        total_old_len += behavior.total_old_len;
        total_new_len += behavior.total_new_len;
        total_retries += behavior.retry_count;
        total_tdd_cycles += behavior.tdd_cycle_count;
        total_breadth += behavior.exploration_breadth();
        total_edit_ops += behavior.edit_op_count;
    }

    // Search-before-act ratio
    let total_ops = total_search + total_action;
    let search_act_ratio = if total_ops > 0 {
        total_search as f64 / total_ops as f64
    } else {
        0.0
    };

    let ratio_color = if search_act_ratio > 0.4 {
        Color32::from_rgb(100, 200, 100)
    } else if search_act_ratio > 0.2 {
        Color32::from_rgb(200, 200, 100)
    } else {
        Color32::from_rgb(200, 100, 100)
    };
    ui.horizontal(|ui| {
        widgets::render_gauge(ui, "Search-before-act", search_act_ratio, ratio_color, 120.0);
        if let Some(def) = metric_registry::lookup("search_before_act") {
            widgets::metric_class_indicator(ui, def);
        }
    });

    ui.add_space(4.0);

    // Edit precision
    let edit_precision = if total_old_len > 0 {
        total_new_len as f64 / total_old_len as f64
    } else {
        1.0
    };
    let precision_display = edit_precision.min(2.0) / 2.0; // normalize to 0-1 for gauge
    let precision_color = if edit_precision < 1.2 && edit_precision > 0.8 {
        Color32::from_rgb(100, 200, 100)
    } else {
        Color32::from_rgb(200, 200, 100)
    };
    ui.horizontal(|ui| {
        widgets::render_gauge(ui, "Edit precision", precision_display, precision_color, 120.0);
        if let Some(def) = metric_registry::lookup("edit_precision") {
            widgets::metric_class_indicator(ui, def);
        }
    });
    widgets::render_metric_row(
        ui,
        "  Edit ratio (new/old)",
        &format!("{:.2}x ({} ops)", edit_precision, total_edit_ops),
        Color32::from_rgb(180, 180, 180),
    );

    ui.add_space(4.0);

    // Confidence score (derived from search-act ratio)
    let confidence = (search_act_ratio * 1.5).min(1.0);
    let conf_color = if confidence > 0.6 {
        Color32::from_rgb(100, 200, 100)
    } else if confidence > 0.3 {
        Color32::from_rgb(200, 200, 100)
    } else {
        Color32::from_rgb(200, 100, 100)
    };
    ui.horizontal(|ui| {
        widgets::render_gauge(ui, "Confidence score", confidence, conf_color, 120.0);
        if let Some(def) = metric_registry::lookup("confidence_score") {
            widgets::metric_class_indicator(ui, def);
        }
    });

    ui.add_space(4.0);

    // Retry count
    let retry_color = if total_retries > 5 {
        Color32::from_rgb(200, 100, 100)
    } else {
        Color32::from_rgb(180, 180, 180)
    };
    ui.horizontal(|ui| {
        widgets::render_metric_row(ui, "Retries", &total_retries.to_string(), retry_color);
        if let Some(def) = metric_registry::lookup("retry_detection") {
            widgets::metric_class_indicator(ui, def);
        }
    });

    // TDD detection
    let tdd_color = if total_tdd_cycles > 0 {
        Color32::from_rgb(100, 200, 100)
    } else {
        Color32::from_rgb(120, 120, 120)
    };
    ui.horizontal(|ui| {
        widgets::render_metric_row(ui, "TDD cycles (T-E-T)", &total_tdd_cycles.to_string(), tdd_color);
        if let Some(def) = metric_registry::lookup("tdd_cycle") {
            widgets::metric_class_indicator(ui, def);
        }
    });

    // Exploration breadth
    ui.horizontal(|ui| {
        widgets::render_metric_row(ui, "Exploration breadth", &format!("{} files", total_breadth), Color32::from_rgb(180, 180, 180));
        if let Some(def) = metric_registry::lookup("exploration_breadth") {
            widgets::metric_class_indicator(ui, def);
        }
    });

    // Per-session breakdown
    if state.session_behaviors.len() > 1 {
        ui.add_space(8.0);
        ui.strong("Per-session");
        ui.add_space(2.0);

        let mut sorted: Vec<_> = state.session_behaviors.iter().collect();
        sorted.sort_by_key(|(_, b)| std::cmp::Reverse(b.search_ops + b.action_ops));

        for (sid, behavior) in sorted.iter().take(10) {
            let short_id = if sid.len() > 12 {
                &sid[..12]
            } else {
                sid
            };
            let ratio = behavior.search_act_ratio();
            ui.horizontal(|ui| {
                ui.label(short_id);
                ui.colored_label(
                    Color32::from_rgb(160, 160, 160),
                    format!(
                        "S/A:{:.0}% retries:{} breadth:{}",
                        ratio * 100.0,
                        behavior.retry_count,
                        behavior.exploration_breadth()
                    ),
                );
            });
        }
    }
}
