use eframe::egui::{self, Color32, Ui};

use crate::metric_registry::{MetricClass, MetricDef};

/// Render a horizontal gauge bar with label and value.
pub fn render_gauge(ui: &mut Ui, label: &str, value: f64, color: Color32, width: f32) {
    ui.horizontal(|ui| {
        ui.label(label);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(format!("{:.0}%", value * 100.0));
            let (rect, _) = ui.allocate_exact_size(
                egui::vec2(width, 12.0),
                egui::Sense::hover(),
            );
            let painter = ui.painter_at(rect);
            painter.rect_filled(rect, 2.0, Color32::from_rgb(60, 60, 60));
            let fill_width = (rect.width() * value.clamp(0.0, 1.0) as f32).max(0.0);
            if fill_width > 0.0 {
                let fill_rect = egui::Rect::from_min_size(
                    rect.min,
                    egui::vec2(fill_width, rect.height()),
                );
                painter.rect_filled(fill_rect, 2.0, color);
            }
        });
    });
}

/// Render a label + right-aligned value in a single row.
pub fn render_metric_row(ui: &mut Ui, label: &str, value_str: &str, color: Color32) {
    ui.horizontal(|ui| {
        ui.label(label);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.colored_label(color, value_str);
        });
    });
}

/// Render a classification indicator after a metric.
/// - Measured: nothing (clean default)
/// - Derived: dim `[d]` with tooltip showing description
/// - Inferred: dim `[~vN]` with tooltip showing formula + version
pub fn metric_class_indicator(ui: &mut Ui, def: &MetricDef) {
    match def.class {
        MetricClass::Measured => {} // no indicator
        MetricClass::Derived => {
            ui.colored_label(Color32::from_rgb(100, 100, 100), "[d]")
                .on_hover_text(format!("{} (Derived)\n{}", def.display_name, def.description));
        }
        MetricClass::Inferred => {
            let text = format!("[~v{}]", def.version);
            ui.colored_label(Color32::from_rgb(100, 100, 100), &text)
                .on_hover_text(format!(
                    "{} (Inferred v{})\n{}",
                    def.display_name, def.version, def.description
                ));
        }
    }
}

/// Render a multi-segment stacked horizontal bar.
pub fn render_stacked_bar(
    ui: &mut Ui,
    segments: &[(f64, Color32, &str)],
    width: f32,
) {
    let total: f64 = segments.iter().map(|(v, _, _)| v).sum();
    if total <= 0.0 {
        return;
    }

    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(width, 16.0),
        egui::Sense::hover(),
    );
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 2.0, Color32::from_rgb(40, 40, 40));

    let mut x = rect.min.x;
    for (value, color, _label) in segments {
        let frac = value / total;
        let seg_width = (rect.width() * frac as f32).max(0.0);
        if seg_width > 0.5 {
            let seg_rect = egui::Rect::from_min_size(
                egui::pos2(x, rect.min.y),
                egui::vec2(seg_width, rect.height()),
            );
            painter.rect_filled(seg_rect, 0.0, *color);
        }
        x += seg_width;
    }
}
