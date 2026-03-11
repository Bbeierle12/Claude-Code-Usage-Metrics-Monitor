use eframe::egui::{self, Color32};

use crate::metric_registry;
use crate::types::MetricsState;
use super::widgets;

pub fn render(ui: &mut egui::Ui, state: &MetricsState) {
    ui.strong("Tool Chains & Patterns");
    ui.add_space(4.0);

    if state.session_behaviors.is_empty() {
        ui.colored_label(Color32::from_rgb(120, 120, 120), "No chain data yet");
        return;
    }

    // Extract trigrams from tool sequences
    let mut trigrams: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
    let mut cooccurrence: std::collections::HashMap<(String, String), u64> = std::collections::HashMap::new();
    let mut total_subagents: u64 = 0;
    let mut subagent_models: std::collections::HashMap<String, u64> = std::collections::HashMap::new();

    for behavior in state.session_behaviors.values() {
        // Trigram extraction
        for seq in &behavior.tool_sequences {
            if seq.len() >= 3 {
                for window in seq.windows(3) {
                    let key = format!("{} -> {} -> {}", window[0], window[1], window[2]);
                    *trigrams.entry(key).or_insert(0) += 1;
                }
            }
        }

        // Co-occurrence aggregation
        for ((a, b), count) in &behavior.tool_cooccurrence {
            *cooccurrence.entry((a.clone(), b.clone())).or_insert(0) += count;
        }

        // Subagent aggregation
        total_subagents += behavior.subagent_count;
        for (model, count) in &behavior.subagent_models {
            *subagent_models.entry(model.clone()).or_insert(0) += count;
        }
    }

    // Top trigrams
    if !trigrams.is_empty() {
        ui.label("Common tool sequences:");
        ui.add_space(2.0);

        let mut sorted: Vec<_> = trigrams.iter().collect();
        sorted.sort_by_key(|(_, c)| std::cmp::Reverse(**c));

        egui::ScrollArea::vertical()
            .id_salt("trigrams")
            .max_height(120.0)
            .show(ui, |ui| {
                for (seq, count) in sorted.iter().take(10) {
                    ui.horizontal(|ui| {
                        ui.colored_label(Color32::from_rgb(160, 200, 255), *seq);
                        ui.colored_label(Color32::from_rgb(120, 120, 120), format!("x{}", count));
                    });
                }
            });

        ui.add_space(8.0);
    }

    // Tool co-occurrence
    if !cooccurrence.is_empty() {
        ui.horizontal(|ui| {
            ui.label("Tool pairs (co-occurrence):");
            if let Some(def) = metric_registry::lookup("tool_cooccurrence") {
                widgets::metric_class_indicator(ui, def);
            }
        });
        ui.add_space(2.0);

        let mut sorted_pairs: Vec<_> = cooccurrence.iter().collect();
        sorted_pairs.sort_by_key(|(_, c)| std::cmp::Reverse(**c));

        for ((a, b), count) in sorted_pairs.iter().take(10) {
            widgets::render_metric_row(
                ui,
                &format!("  {} + {}", a, b),
                &count.to_string(),
                Color32::from_rgb(180, 180, 180),
            );
        }

        ui.add_space(8.0);
    }

    // Subagent summary
    widgets::render_metric_row(
        ui,
        "Subagent spawns",
        &total_subagents.to_string(),
        Color32::from_rgb(180, 180, 180),
    );

    if !subagent_models.is_empty() {
        ui.add_space(2.0);
        for (model, count) in &subagent_models {
            widgets::render_metric_row(
                ui,
                &format!("  {}", model),
                &count.to_string(),
                Color32::from_rgb(160, 160, 160),
            );
        }
    }
}
