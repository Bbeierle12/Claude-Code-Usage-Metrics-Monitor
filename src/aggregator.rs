use chrono::Utc;

use crate::config;
use crate::parser;
use crate::types::{MessageRecord, MetricsState, ModelMetrics, ProjectMetrics, SessionMetrics};

impl MetricsState {
    /// Ingest a batch of new message records.
    pub fn ingest(&mut self, records: &[MessageRecord]) {
        let today = Utc::now().date_naive();

        for rec in records {
            // Only count today's records
            if rec.timestamp.date_naive() != today {
                continue;
            }

            self.total_messages += 1;
            self.total_input += rec.input_tokens;
            self.total_output += rec.output_tokens;
            self.total_cache_creation += rec.cache_creation_tokens;
            self.total_cache_read += rec.cache_read_tokens;

            // Update last_updated
            match self.last_updated {
                Some(prev) if rec.timestamp > prev => self.last_updated = Some(rec.timestamp),
                None => self.last_updated = Some(rec.timestamp),
                _ => {}
            }

            // Per-session
            let session = self
                .sessions
                .entry(rec.session_id.clone())
                .or_insert_with(|| SessionMetrics {
                    session_id: rec.session_id.clone(),
                    project: parser::short_project_name(&rec.cwd),
                    model: rec.model.clone(),
                    first_seen: rec.timestamp,
                    last_seen: rec.timestamp,
                    input_tokens: 0,
                    output_tokens: 0,
                    cache_creation_tokens: 0,
                    cache_read_tokens: 0,
                    message_count: 0,
                });
            session.input_tokens += rec.input_tokens;
            session.output_tokens += rec.output_tokens;
            session.cache_creation_tokens += rec.cache_creation_tokens;
            session.cache_read_tokens += rec.cache_read_tokens;
            session.message_count += 1;
            if rec.timestamp > session.last_seen {
                session.last_seen = rec.timestamp;
            }
            if rec.timestamp < session.first_seen {
                session.first_seen = rec.timestamp;
            }
            // Use the latest model seen
            if !rec.model.is_empty() && rec.model != "unknown" {
                session.model = rec.model.clone();
            }

            // Per-project
            let project_name = parser::short_project_name(&rec.cwd);
            let project = self
                .projects
                .entry(project_name.clone())
                .or_insert_with(|| ProjectMetrics {
                    name: project_name,
                    ..Default::default()
                });
            project.input_tokens += rec.input_tokens;
            project.output_tokens += rec.output_tokens;
            project.cache_creation_tokens += rec.cache_creation_tokens;
            project.cache_read_tokens += rec.cache_read_tokens;
            // Count unique sessions per project (increment only on first record for session)
            // This is approximate — fine for a widget
            if session.message_count == 1 {
                project.session_count += 1;
            }

            // Per-model
            let model_key = friendly_model_name(&rec.model);
            let model_metrics = self
                .models
                .entry(model_key.to_string())
                .or_insert_with(ModelMetrics::default);
            model_metrics.input_tokens += rec.input_tokens;
            model_metrics.output_tokens += rec.output_tokens;
            model_metrics.cache_creation_tokens += rec.cache_creation_tokens;
            model_metrics.cache_read_tokens += rec.cache_read_tokens;
            model_metrics.message_count += 1;
        }
    }

    /// Total estimated cost across all models today.
    pub fn estimated_cost(&self) -> f64 {
        let mut cost = 0.0;
        for (model_name, m) in &self.models {
            cost += config::estimate_cost(
                model_name,
                m.input_tokens,
                m.output_tokens,
                m.cache_creation_tokens,
                m.cache_read_tokens,
            );
        }
        cost
    }

    /// Number of sessions active within the threshold.
    pub fn active_session_count(&self) -> usize {
        let now = Utc::now();
        self.sessions
            .values()
            .filter(|s| s.is_active(now, config::ACTIVE_SESSION_THRESHOLD_MINUTES))
            .count()
    }

    /// Sessions sorted by last_seen descending.
    pub fn sessions_sorted(&self) -> Vec<&SessionMetrics> {
        let mut sessions: Vec<_> = self.sessions.values().collect();
        sessions.sort_by(|a, b| b.last_seen.cmp(&a.last_seen));
        sessions
    }

    /// Projects sorted by total tokens descending.
    pub fn projects_sorted(&self) -> Vec<&ProjectMetrics> {
        let mut projects: Vec<_> = self.projects.values().collect();
        projects.sort_by(|a, b| b.total_tokens().cmp(&a.total_tokens()));
        projects
    }
}

fn friendly_model_name(model: &str) -> &str {
    if model.contains("opus") {
        "opus"
    } else if model.contains("sonnet") {
        "sonnet"
    } else if model.contains("haiku") {
        "haiku"
    } else {
        model
    }
}
