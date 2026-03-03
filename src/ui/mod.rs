pub mod projects;
pub mod sessions;
pub mod summary;

use eframe::egui;

use crate::types::MetricsState;

pub fn render(ctx: &egui::Context, state: &MetricsState) {
    egui::CentralPanel::default().show(ctx, |ui| {
        ui.heading("Claude Code Usage");
        ui.add_space(4.0);

        if state.total_messages == 0 {
            ui.centered_and_justified(|ui| {
                ui.colored_label(
                    egui::Color32::from_rgb(120, 120, 120),
                    "No sessions detected today",
                );
            });
            return;
        }

        summary::render(ui, state);
        ui.add_space(4.0);
        sessions::render(ui, state);
        ui.add_space(8.0);
        ui.separator();
        ui.add_space(4.0);
        projects::render(ui, state);
    });
}
