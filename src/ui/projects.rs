use eframe::egui;

use crate::types::MetricsState;

pub fn render(ui: &mut egui::Ui, state: &MetricsState) {
    let projects = state.projects_sorted();

    if projects.is_empty() {
        return;
    }

    ui.strong(format!("Projects ({})", projects.len()));
    ui.add_space(4.0);

    let max_tokens = projects.first().map(|p| p.total_tokens()).unwrap_or(1).max(1);

    egui::ScrollArea::vertical()
        .max_height(160.0)
        .show(ui, |ui| {
            for p in &projects {
                ui.horizontal(|ui| {
                    // Project name
                    let name = if p.name.len() > 24 {
                        format!("{}...", &p.name[..21])
                    } else {
                        p.name.clone()
                    };
                    ui.label(format!("{:<24}", name));

                    // Bar
                    let frac = p.total_tokens() as f32 / max_tokens as f32;
                    let bar_width = 150.0;
                    let (rect, _) =
                        ui.allocate_exact_size(egui::vec2(bar_width, 14.0), egui::Sense::hover());

                    let painter = ui.painter();
                    // Background
                    painter.rect_filled(
                        rect,
                        2.0,
                        egui::Color32::from_rgb(40, 40, 50),
                    );
                    // Fill
                    let fill_rect = egui::Rect::from_min_size(
                        rect.min,
                        egui::vec2(bar_width * frac, 14.0),
                    );
                    painter.rect_filled(
                        fill_rect,
                        2.0,
                        egui::Color32::from_rgb(80, 160, 255),
                    );

                    // Token count label
                    ui.label(format_tokens(p.total_tokens()));
                });
            }
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
