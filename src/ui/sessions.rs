use chrono::Utc;
use eframe::egui;
use std::sync::mpsc;

use crate::settings::Settings;
use crate::types::{format_tokens, MetricsState};

use super::timeline;

/// Expanded session detail state. Stored externally and passed in.
pub struct SessionDetailState {
    pub expanded_session: Option<String>,
    pub timeline_events: Vec<timeline::TimelineEvent>,
    pub projects_dir: std::path::PathBuf,
    /// Receiver for background timeline loading results.
    timeline_rx: Option<mpsc::Receiver<Vec<timeline::TimelineEvent>>>,
    /// True while waiting for background load to complete.
    pub loading: bool,
}

impl SessionDetailState {
    pub fn new(projects_dir: std::path::PathBuf) -> Self {
        Self {
            expanded_session: None,
            timeline_events: Vec::new(),
            projects_dir,
            timeline_rx: None,
            loading: false,
        }
    }

    pub fn toggle(&mut self, session_id: &str) {
        if self.expanded_session.as_deref() == Some(session_id) {
            self.expanded_session = None;
            self.timeline_events.clear();
            self.timeline_rx = None;
            self.loading = false;
        } else {
            self.expanded_session = Some(session_id.to_string());
            self.timeline_events.clear();
            self.loading = true;

            // Load timeline in a background thread to avoid blocking the UI.
            let (tx, rx) = mpsc::channel();
            let dir = self.projects_dir.clone();
            let sid = session_id.to_string();
            std::thread::spawn(move || {
                let events = timeline::load_session_timeline(&dir, &sid);
                let _ = tx.send(events);
            });
            self.timeline_rx = Some(rx);
        }
    }

    /// Poll for background timeline results. Call once per frame.
    pub fn poll(&mut self) {
        if let Some(ref rx) = self.timeline_rx {
            if let Ok(events) = rx.try_recv() {
                self.timeline_events = events;
                self.loading = false;
                self.timeline_rx = None;
            }
        }
    }
}

pub fn render(ui: &mut egui::Ui, state: &MetricsState, detail: &mut SessionDetailState, settings: &Settings) {
    // Poll for background timeline load results
    detail.poll();

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
        .id_salt("sessions_scroll")
        .max_height(200.0)
        .show(ui, |ui| {
            egui::Grid::new("sessions_grid")
                .num_columns(6)
                .spacing([12.0, 4.0])
                .striped(true)
                .show(ui, |ui| {
                    // Header
                    ui.strong("Project");
                    ui.strong("Model");
                    ui.strong("Duration");
                    ui.strong("Tokens");
                    ui.strong("Cost");
                    ui.strong("Branch");
                    ui.end_row();

                    for (session_id, s) in &sessions {
                        let session_id: &str = session_id;
                        let is_active =
                            s.is_active(now, settings.active_session_threshold_minutes);
                        let is_expanded = detail.expanded_session.as_deref() == Some(session_id);
                        let text_color = if is_active {
                            egui::Color32::from_rgb(200, 230, 255)
                        } else {
                            egui::Color32::from_rgb(180, 180, 180)
                        };

                        // Project name (clickable)
                        let proj = if s.project.len() > 20 {
                            format!("{}...", &s.project[..17])
                        } else {
                            s.project.clone()
                        };
                        let prefix = if is_expanded { "v " } else { "> " };
                        if ui
                            .add(egui::Label::new(
                                egui::RichText::new(format!("{}{}", prefix, proj)).color(text_color),
                            ).sense(egui::Sense::click()))
                            .clicked()
                        {
                            detail.toggle(session_id);
                        }

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
                        let cost = settings.estimate_cost(
                            &s.model,
                            s.input_tokens,
                            s.output_tokens,
                            s.cache_creation_tokens,
                            s.cache_read_tokens,
                        );
                        ui.colored_label(text_color, format!("${:.2}", cost));

                        // Branch
                        let branch_label = if s.branch.is_empty() {
                            "-".to_string()
                        } else if s.branch.len() > 18 {
                            format!("{}...", &s.branch[..15])
                        } else {
                            s.branch.clone()
                        };
                        ui.colored_label(text_color, branch_label);

                        ui.end_row();

                        // Expanded timeline detail
                        if is_expanded {
                            ui.label(""); // col 1 padding
                            ui.end_row();
                            if detail.loading {
                                ui.colored_label(
                                    egui::Color32::from_rgb(150, 150, 150),
                                    "Loading timeline...",
                                );
                            } else if !detail.timeline_events.is_empty() {
                                timeline::render(ui, &detail.timeline_events, session_id);
                            }
                            ui.end_row();
                        }
                    }
                });
        });
}
