use eframe::egui::{self, Color32};

use crate::metric_registry;
use crate::types::{MetricsState, SessionPhase};
use super::widgets;

pub fn render(ui: &mut egui::Ui, state: &MetricsState) {
    ui.strong("Conversation Structure");
    ui.add_space(4.0);

    if state.session_behaviors.is_empty() {
        ui.colored_label(Color32::from_rgb(120, 120, 120), "No conversation data yet");
        return;
    }

    // Aggregated metrics
    let mut max_depth: u32 = 0;
    let mut total_branches: u32 = 0;
    let mut total_compactions: u64 = 0;
    let mut total_prompts: u64 = 0;
    let mut total_prompt_len: u64 = 0;
    let mut total_questions: u64 = 0;
    let mut total_directives: u64 = 0;

    for behavior in state.session_behaviors.values() {
        max_depth = max_depth.max(behavior.max_tree_depth);
        total_branches += behavior.branch_count;
        total_compactions += behavior.compaction_count;
        total_questions += behavior.question_count;
        total_directives += behavior.directive_count;
        for &len in &behavior.prompt_lengths {
            total_prompts += 1;
            total_prompt_len += len;
        }
    }

    // Conversation tree depth
    let depth_ratio = (max_depth as f64 / 20.0).min(1.0);
    ui.horizontal(|ui| {
        widgets::render_gauge(ui, "Tree depth", depth_ratio, Color32::from_rgb(100, 180, 255), 120.0);
        if let Some(def) = metric_registry::lookup("tree_depth") {
            widgets::metric_class_indicator(ui, def);
        }
    });
    widgets::render_metric_row(
        ui,
        "  Max depth",
        &max_depth.to_string(),
        Color32::from_rgb(180, 180, 180),
    );

    ui.add_space(4.0);

    // Branch count (conversation forks)
    widgets::render_metric_row(
        ui,
        "Conversation forks",
        &total_branches.to_string(),
        Color32::from_rgb(180, 180, 180),
    );

    // Compaction frequency
    widgets::render_metric_row(
        ui,
        "Compactions",
        &total_compactions.to_string(),
        Color32::from_rgb(180, 180, 180),
    );

    ui.add_space(8.0);

    // Phase timeline
    ui.horizontal(|ui| {
        ui.label("Session phases:");
        if let Some(def) = metric_registry::lookup("session_phase") {
            widgets::metric_class_indicator(ui, def);
        }
    });
    ui.add_space(2.0);

    for (sid, behavior) in &state.session_behaviors {
        if behavior.phase_transitions.is_empty() {
            continue;
        }
        let short_id = if sid.len() > 12 { &sid[..12] } else { sid };
        ui.horizontal(|ui| {
            ui.label(short_id);
            for (_ts, phase) in &behavior.phase_transitions {
                let (color, label) = match phase {
                    SessionPhase::Explore => (Color32::from_rgb(100, 150, 255), "E"),
                    SessionPhase::Plan => (Color32::from_rgb(100, 220, 220), "P"),
                    SessionPhase::Implement => (Color32::from_rgb(100, 220, 100), "I"),
                    SessionPhase::Verify => (Color32::from_rgb(220, 220, 100), "V"),
                    SessionPhase::Unknown => (Color32::from_rgb(120, 120, 120), "?"),
                };
                ui.colored_label(color, label);
            }
        });
    }

    ui.add_space(8.0);

    // Prompt length distribution
    let avg_prompt = if total_prompts > 0 {
        total_prompt_len as f64 / total_prompts as f64
    } else {
        0.0
    };
    widgets::render_metric_row(
        ui,
        "Avg prompt length",
        &format!("{:.0} chars", avg_prompt),
        Color32::from_rgb(180, 180, 180),
    );

    // Question vs directive
    ui.horizontal(|ui| {
        widgets::render_metric_row(ui, "Questions (long)", &total_questions.to_string(), Color32::from_rgb(180, 180, 180));
        widgets::render_metric_row(ui, "Directives (short)", &total_directives.to_string(), Color32::from_rgb(180, 180, 180));
        if let Some(def) = metric_registry::lookup("prompt_intent") {
            widgets::metric_class_indicator(ui, def);
        }
    });

    // Response density
    let mut total_assistant_tokens: u64 = 0;
    let mut total_assistant_msgs: u64 = 0;
    for session in state.sessions.values() {
        total_assistant_tokens += session.output_tokens;
        total_assistant_msgs += session.assistant_message_count;
    }
    let tokens_per_msg = if total_assistant_msgs > 0 {
        total_assistant_tokens as f64 / total_assistant_msgs as f64
    } else {
        0.0
    };
    widgets::render_metric_row(
        ui,
        "Output tokens/msg",
        &format!("{:.0}", tokens_per_msg),
        Color32::from_rgb(180, 180, 180),
    );
}
