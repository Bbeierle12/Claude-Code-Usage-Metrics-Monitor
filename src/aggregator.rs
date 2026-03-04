use std::cmp::Reverse;

use chrono::{Duration, Utc};

use crate::parser;
use crate::settings::Settings;
use crate::types::{
    BranchMetrics, MessageRecord, MessageType, MetricsState, PendingToolUse, ProjectMetrics,
    SessionMetrics,
};

impl MetricsState {
    /// Ingest a batch of new message records (assistant, user prompt, or tool result).
    /// Does NOT prune the burn window — call `prune_burn_window()` on the main thread.
    ///
    /// Pass `idle_gap_minutes` from settings to detect idle gaps between messages.
    /// Use 0 to disable idle gap detection.
    pub fn ingest(&mut self, records: &[MessageRecord], idle_gap_minutes: i64) {
        let today = Utc::now().date_naive();

        for rec in records {
            // Only count today's records
            if rec.timestamp.date_naive() != today {
                continue;
            }

            self.dirty = true;
            self.total_messages += 1;

            // Update last_updated
            match self.last_updated {
                Some(prev) if rec.timestamp > prev => self.last_updated = Some(rec.timestamp),
                None => self.last_updated = Some(rec.timestamp),
                _ => {}
            }

            // Per-session (common to all types)
            let session = self
                .sessions
                .entry(rec.session_id.clone())
                .or_insert_with(|| SessionMetrics {
                    project: parser::short_project_name(&rec.cwd),
                    model: rec.model.clone(),
                    first_seen: rec.timestamp,
                    last_seen: rec.timestamp,
                    input_tokens: 0,
                    output_tokens: 0,
                    cache_creation_tokens: 0,
                    cache_read_tokens: 0,
                    message_count: 0,
                    branch: String::new(),
                    user_message_count: 0,
                    tool_result_count: 0,
                    tool_error_count: 0,
                    assistant_text_length: 0,
                    user_text_length: 0,
                    assistant_message_count: 0,
                    turn_count: 0,
                    idle_gap_count: 0,
                    total_idle_secs: 0,
                    assistant_word_count: 0,
                    user_word_count: 0,
                });

            // Idle gap detection: compare with previous last_seen BEFORE updating
            if session.message_count > 0 && idle_gap_minutes > 0 {
                let gap_secs = (rec.timestamp - session.last_seen).num_seconds();
                let threshold_secs = idle_gap_minutes * 60;
                if gap_secs > threshold_secs {
                    session.idle_gap_count += 1;
                    session.total_idle_secs += gap_secs;
                }
            }

            session.message_count += 1;
            if rec.timestamp > session.last_seen {
                session.last_seen = rec.timestamp;
            }
            if rec.timestamp < session.first_seen {
                session.first_seen = rec.timestamp;
            }
            // Use the latest non-empty branch
            if !rec.git_branch.is_empty() {
                session.branch = rec.git_branch.clone();
            }

            // Per-project (common: create entry, count sessions)
            let project_name = parser::short_project_name(&rec.cwd);
            let project = self
                .projects
                .entry(project_name.clone())
                .or_insert_with(|| ProjectMetrics {
                    name: project_name,
                    ..Default::default()
                });
            if session.message_count == 1 {
                project.session_count += 1;
            }

            // Type-specific accumulation
            match rec.message_type {
                MessageType::Assistant => {
                    session.assistant_message_count += 1;

                    // Token accounting (only assistant lines have usage data)
                    self.total_input += rec.input_tokens;
                    self.total_output += rec.output_tokens;
                    self.total_cache_creation += rec.cache_creation_tokens;
                    self.total_cache_read += rec.cache_read_tokens;

                    session.input_tokens += rec.input_tokens;
                    session.output_tokens += rec.output_tokens;
                    session.cache_creation_tokens += rec.cache_creation_tokens;
                    session.cache_read_tokens += rec.cache_read_tokens;
                    session.assistant_text_length += rec.text_length;
                    session.assistant_word_count += rec.text_word_count;

                    // Use the latest model seen
                    if !rec.model.is_empty() && rec.model != "unknown" {
                        session.model = rec.model.clone();
                    }

                    // Per-tool invocation counting + stash pending for latency correlation
                    for (i, tool_name) in rec.tool_names.iter().enumerate() {
                        *self.tools.entry(tool_name.clone()).or_insert(0) += 1;

                        // Stash pending tool_use for latency correlation
                        if let Some(tool_use_id) = rec.tool_use_ids.get(i) {
                            self.pending_tool_uses.insert(
                                tool_use_id.clone(),
                                PendingToolUse {
                                    timestamp: rec.timestamp,
                                    tool_name: tool_name.clone(),
                                    session_id: rec.session_id.clone(),
                                },
                            );
                        }
                    }

                    // Per-branch (token accounting)
                    if !rec.git_branch.is_empty() {
                        let branch = self
                            .branches
                            .entry(rec.git_branch.clone())
                            .or_insert_with(|| BranchMetrics {
                                name: rec.git_branch.clone(),
                                ..Default::default()
                            });
                        branch.input_tokens += rec.input_tokens;
                        branch.output_tokens += rec.output_tokens;
                        branch.cache_creation_tokens += rec.cache_creation_tokens;
                        branch.cache_read_tokens += rec.cache_read_tokens;
                        branch.message_count += 1;
                    }

                    // Burn window (output tokens)
                    self.burn_window
                        .push_back((rec.timestamp, rec.output_tokens));

                    // Per-project token accounting
                    project.input_tokens += rec.input_tokens;
                    project.output_tokens += rec.output_tokens;
                    project.cache_creation_tokens += rec.cache_creation_tokens;
                    project.cache_read_tokens += rec.cache_read_tokens;

                    // Per-model (preserve full model identifier)
                    let model_metrics =
                        self.models.entry(rec.model.clone()).or_default();
                    model_metrics.input_tokens += rec.input_tokens;
                    model_metrics.output_tokens += rec.output_tokens;
                    model_metrics.cache_creation_tokens += rec.cache_creation_tokens;
                    model_metrics.cache_read_tokens += rec.cache_read_tokens;
                    model_metrics.message_count += 1;
                }
                MessageType::UserPrompt => {
                    session.user_message_count += 1;
                    session.user_text_length += rec.text_length;
                    session.user_word_count += rec.text_word_count;
                    session.turn_count += 1;
                }
                MessageType::ToolResult => {
                    session.tool_result_count += 1;
                    let is_error = rec.is_tool_error == Some(true);
                    if is_error {
                        session.tool_error_count += 1;
                    }

                    // Correlate with pending tool_use for latency
                    for tool_use_id in &rec.tool_use_ids {
                        if let Some(pending) = self.pending_tool_uses.remove(tool_use_id) {
                            let latency_ms =
                                (rec.timestamp - pending.timestamp).num_milliseconds();
                            self.tool_latencies
                                .entry(pending.tool_name)
                                .or_default()
                                .record(latency_ms, is_error);
                        }
                    }
                }
            }
        }
    }

    /// Prune burn window entries older than `window_minutes`. Call on the main thread.
    pub fn prune_burn_window(&mut self, window_minutes: i64) {
        let cutoff = Utc::now() - Duration::minutes(window_minutes);
        while self
            .burn_window
            .front()
            .is_some_and(|(ts, _)| *ts < cutoff)
        {
            self.burn_window.pop_front();
        }
    }

    /// Total estimated cost across all models today.
    pub fn estimated_cost(&self, settings: &Settings) -> f64 {
        let mut cost = 0.0;
        for (model_name, m) in &self.models {
            cost += settings.estimate_cost(
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
    pub fn active_session_count(&self, settings: &Settings) -> usize {
        let now = Utc::now();
        self.sessions
            .values()
            .filter(|s| s.is_active(now, settings.active_session_threshold_minutes))
            .count()
    }

    /// Sessions sorted by last_seen descending.
    pub fn sessions_sorted(&self) -> Vec<&SessionMetrics> {
        let mut sessions: Vec<_> = self.sessions.values().collect();
        sessions.sort_by_key(|s| Reverse(s.last_seen));
        sessions
    }

    /// Projects sorted by total tokens descending.
    pub fn projects_sorted(&self) -> Vec<&ProjectMetrics> {
        let mut projects: Vec<_> = self.projects.values().collect();
        projects.sort_by_key(|p| Reverse(p.total_tokens()));
        projects
    }

    /// Tools sorted by invocation count descending.
    pub fn tools_sorted(&self) -> Vec<(&String, &u64)> {
        let mut tools: Vec<_> = self.tools.iter().collect();
        tools.sort_by_key(|(_, count)| Reverse(*count));
        tools
    }

    /// Branches sorted by total tokens descending.
    pub fn branches_sorted(&self) -> Vec<&BranchMetrics> {
        let mut branches: Vec<_> = self.branches.values().collect();
        branches.sort_by_key(|b| Reverse(b.total_tokens()));
        branches
    }

    /// Output tokens per minute over the burn window.
    pub fn burn_rate_per_minute(&self, settings: &Settings) -> f64 {
        if self.burn_window.is_empty() {
            return 0.0;
        }
        let total: u64 = self.burn_window.iter().map(|(_, tokens)| tokens).sum();
        let window_minutes = settings.burn_rate_window_minutes as f64;
        total as f64 / window_minutes
    }

    /// Effective burn rate: tokens/min excluding idle gaps in the burn window.
    #[allow(dead_code)] // available for UI integration
    /// Falls back to `burn_rate_per_minute` if no idle gaps detected.
    pub fn effective_burn_rate(&self, settings: &Settings) -> f64 {
        if self.burn_window.len() < 2 {
            return self.burn_rate_per_minute(settings);
        }

        let total: u64 = self.burn_window.iter().map(|(_, tokens)| tokens).sum();
        let idle_threshold_secs = settings.idle_gap_minutes * 60;

        // Sum "active" time: consecutive gaps ≤ threshold
        let mut active_secs: i64 = 0;
        let entries: Vec<_> = self.burn_window.iter().collect();
        for pair in entries.windows(2) {
            let gap = (pair[1].0 - pair[0].0).num_seconds();
            if gap <= idle_threshold_secs {
                active_secs += gap;
            }
        }

        if active_secs <= 0 {
            return self.burn_rate_per_minute(settings);
        }

        let active_minutes = active_secs as f64 / 60.0;
        total as f64 / active_minutes
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::Settings;
    use crate::types::{MessageRecord, MetricsState};
    use chrono::{DateTime, Utc};

    fn make_record(session: &str, model: &str, input: u64, output: u64) -> MessageRecord {
        let home = dirs::home_dir().unwrap().to_string_lossy().to_string();
        MessageRecord {
            session_id: session.to_string(),
            timestamp: Utc::now(),
            cwd: format!("{}/test-project", home),
            model: model.to_string(),
            input_tokens: input,
            output_tokens: output,
            cache_creation_tokens: 0,
            cache_read_tokens: 0,
            tool_names: vec![],
            git_branch: String::new(),
            message_type: MessageType::Assistant,
            uuid: String::new(),
            parent_uuid: String::new(),
            text_length: 0,
            text_word_count: 0,
            tool_use_ids: vec![],
            is_tool_error: None,
        }
    }

    #[test]
    fn test_ingest_counts_tokens() {
        let mut state = MetricsState::default();
        let records = vec![
            make_record("s1", "claude-sonnet-4-5", 100, 200),
            make_record("s1", "claude-sonnet-4-5", 150, 250),
        ];
        state.ingest(&records, 0);

        assert_eq!(state.total_input, 250);
        assert_eq!(state.total_output, 450);
        assert_eq!(state.total_messages, 2);
    }

    #[test]
    fn test_ingest_separates_sessions() {
        let mut state = MetricsState::default();
        let records = vec![
            make_record("s1", "claude-sonnet-4-5", 100, 200),
            make_record("s2", "claude-opus-4-5", 300, 400),
        ];
        state.ingest(&records, 0);

        assert_eq!(state.sessions.len(), 2);
        assert_eq!(state.sessions["s1"].input_tokens, 100);
        assert_eq!(state.sessions["s2"].input_tokens, 300);
    }

    #[test]
    fn test_ingest_groups_by_model() {
        let mut state = MetricsState::default();
        let records = vec![
            make_record("s1", "claude-sonnet-4-5", 100, 200),
            make_record("s2", "claude-opus-4-5", 300, 400),
        ];
        state.ingest(&records, 0);

        assert_eq!(state.models.len(), 2);
        assert!(state.models.contains_key("claude-sonnet-4-5"));
        assert!(state.models.contains_key("claude-opus-4-5"));
    }

    #[test]
    fn test_estimated_cost_nonzero() {
        let mut state = MetricsState::default();
        let records = vec![make_record("s1", "claude-sonnet-4-5", 1_000_000, 500_000)];
        state.ingest(&records, 0);

        let cost = state.estimated_cost(&Settings::default());
        assert!(cost > 0.0, "cost should be positive, got {}", cost);
    }

    #[test]
    fn test_model_keys_preserve_full_name() {
        let mut state = MetricsState::default();
        let records = vec![
            make_record("s1", "claude-opus-4-5", 100, 200),
            make_record("s2", "claude-opus-4-6", 300, 400),
        ];
        state.ingest(&records, 0);

        // Different model versions should be separate entries
        assert_eq!(state.models.len(), 2);
        assert!(state.models.contains_key("claude-opus-4-5"));
        assert!(state.models.contains_key("claude-opus-4-6"));
    }

    #[test]
    fn test_ingest_skips_yesterday() {
        let mut state = MetricsState::default();
        let home = dirs::home_dir().unwrap().to_string_lossy().to_string();
        let yesterday = Utc::now() - chrono::Duration::days(1);
        let records = vec![MessageRecord {
            session_id: "old".to_string(),
            timestamp: yesterday,
            cwd: format!("{}/old-proj", home),
            model: "claude-sonnet-4-5".to_string(),
            input_tokens: 500,
            output_tokens: 500,
            cache_creation_tokens: 0,
            cache_read_tokens: 0,
            tool_names: vec![],
            git_branch: String::new(),
            message_type: MessageType::Assistant,
            uuid: String::new(),
            parent_uuid: String::new(),
            text_length: 0,
            text_word_count: 0,
            tool_use_ids: vec![],
            is_tool_error: None,
        }];
        state.ingest(&records, 0);

        assert_eq!(state.total_input, 0);
        assert_eq!(state.total_output, 0);
        assert_eq!(state.total_messages, 0);
    }

    #[test]
    fn test_ingest_updates_last_updated() {
        let mut state = MetricsState::default();
        let records = vec![make_record("s1", "claude-sonnet-4-5", 100, 200)];
        let ts = records[0].timestamp;
        state.ingest(&records, 0);

        assert_eq!(state.last_updated, Some(ts));
    }

    #[test]
    fn test_ingest_session_model_upgrade() {
        let mut state = MetricsState::default();

        // First record with "unknown" model
        let home = dirs::home_dir().unwrap().to_string_lossy().to_string();
        let rec1 = MessageRecord {
            session_id: "s1".to_string(),
            timestamp: Utc::now(),
            cwd: format!("{}/test-project", home),
            model: "unknown".to_string(),
            input_tokens: 10,
            output_tokens: 20,
            cache_creation_tokens: 0,
            cache_read_tokens: 0,
            tool_names: vec![],
            git_branch: String::new(),
            message_type: MessageType::Assistant,
            uuid: String::new(),
            parent_uuid: String::new(),
            text_length: 0,
            text_word_count: 0,
            tool_use_ids: vec![],
            is_tool_error: None,
        };
        state.ingest(&[rec1], 0);
        assert_eq!(state.sessions["s1"].model, "unknown");

        // Second record with real model
        let rec2 = MessageRecord {
            session_id: "s1".to_string(),
            timestamp: Utc::now(),
            cwd: format!("{}/test-project", home),
            model: "claude-sonnet-4-5".to_string(),
            input_tokens: 30,
            output_tokens: 40,
            cache_creation_tokens: 0,
            cache_read_tokens: 0,
            tool_names: vec![],
            git_branch: String::new(),
            message_type: MessageType::Assistant,
            uuid: String::new(),
            parent_uuid: String::new(),
            text_length: 0,
            text_word_count: 0,
            tool_use_ids: vec![],
            is_tool_error: None,
        };
        state.ingest(&[rec2], 0);
        assert_eq!(state.sessions["s1"].model, "claude-sonnet-4-5");
    }

    #[test]
    fn test_sessions_sorted_order() {
        let mut state = MetricsState::default();
        let home = dirs::home_dir().unwrap().to_string_lossy().to_string();
        let now = Utc::now();

        // Three sessions with staggered timestamps (oldest first in ingestion)
        let records = vec![
            MessageRecord {
                session_id: "oldest".to_string(),
                timestamp: now - chrono::Duration::minutes(30),
                cwd: format!("{}/proj", home),
                model: "sonnet".to_string(),
                input_tokens: 10,
                output_tokens: 10,
                cache_creation_tokens: 0,
                cache_read_tokens: 0,
                tool_names: vec![],
                git_branch: String::new(),
                message_type: MessageType::Assistant,
                uuid: String::new(),
                parent_uuid: String::new(),
                text_length: 0,
                text_word_count: 0,
                tool_use_ids: vec![],
                is_tool_error: None,
            },
            MessageRecord {
                session_id: "middle".to_string(),
                timestamp: now - chrono::Duration::minutes(15),
                cwd: format!("{}/proj", home),
                model: "sonnet".to_string(),
                input_tokens: 20,
                output_tokens: 20,
                cache_creation_tokens: 0,
                cache_read_tokens: 0,
                tool_names: vec![],
                git_branch: String::new(),
                message_type: MessageType::Assistant,
                uuid: String::new(),
                parent_uuid: String::new(),
                text_length: 0,
                text_word_count: 0,
                tool_use_ids: vec![],
                is_tool_error: None,
            },
            MessageRecord {
                session_id: "newest".to_string(),
                timestamp: now,
                cwd: format!("{}/proj", home),
                model: "sonnet".to_string(),
                input_tokens: 30,
                output_tokens: 30,
                cache_creation_tokens: 0,
                cache_read_tokens: 0,
                tool_names: vec![],
                git_branch: String::new(),
                message_type: MessageType::Assistant,
                uuid: String::new(),
                parent_uuid: String::new(),
                text_length: 0,
                text_word_count: 0,
                tool_use_ids: vec![],
                is_tool_error: None,
            },
        ];
        state.ingest(&records, 0);

        let sorted = state.sessions_sorted();
        assert_eq!(sorted.len(), 3);
        // Most recent first
        assert_eq!(sorted[0].input_tokens, 30); // newest
        assert_eq!(sorted[1].input_tokens, 20); // middle
        assert_eq!(sorted[2].input_tokens, 10); // oldest
    }

    #[test]
    fn test_projects_sorted_order() {
        let mut state = MetricsState::default();
        let home = dirs::home_dir().unwrap().to_string_lossy().to_string();

        // Two projects with different token counts
        let records = vec![
            MessageRecord {
                session_id: "s1".to_string(),
                timestamp: Utc::now(),
                cwd: format!("{}/small-proj", home),
                model: "sonnet".to_string(),
                input_tokens: 100,
                output_tokens: 100,
                cache_creation_tokens: 0,
                cache_read_tokens: 0,
                tool_names: vec![],
                git_branch: String::new(),
                message_type: MessageType::Assistant,
                uuid: String::new(),
                parent_uuid: String::new(),
                text_length: 0,
                text_word_count: 0,
                tool_use_ids: vec![],
                is_tool_error: None,
            },
            MessageRecord {
                session_id: "s2".to_string(),
                timestamp: Utc::now(),
                cwd: format!("{}/big-proj", home),
                model: "sonnet".to_string(),
                input_tokens: 1000,
                output_tokens: 1000,
                cache_creation_tokens: 0,
                cache_read_tokens: 0,
                tool_names: vec![],
                git_branch: String::new(),
                message_type: MessageType::Assistant,
                uuid: String::new(),
                parent_uuid: String::new(),
                text_length: 0,
                text_word_count: 0,
                tool_use_ids: vec![],
                is_tool_error: None,
            },
        ];
        state.ingest(&records, 0);

        let sorted = state.projects_sorted();
        assert_eq!(sorted.len(), 2);
        assert_eq!(sorted[0].name, "big-proj"); // highest tokens first
        assert_eq!(sorted[1].name, "small-proj");
    }

    #[test]
    fn test_active_session_count() {
        let mut state = MetricsState::default();
        let home = dirs::home_dir().unwrap().to_string_lossy().to_string();
        let now = Utc::now();

        let records = vec![
            // Active — timestamp is now
            MessageRecord {
                session_id: "active".to_string(),
                timestamp: now,
                cwd: format!("{}/proj", home),
                model: "sonnet".to_string(),
                input_tokens: 10,
                output_tokens: 10,
                cache_creation_tokens: 0,
                cache_read_tokens: 0,
                tool_names: vec![],
                git_branch: String::new(),
                message_type: MessageType::Assistant,
                uuid: String::new(),
                parent_uuid: String::new(),
                text_length: 0,
                text_word_count: 0,
                tool_use_ids: vec![],
                is_tool_error: None,
            },
            // Stale — 30 minutes ago
            MessageRecord {
                session_id: "stale".to_string(),
                timestamp: now - chrono::Duration::minutes(30),
                cwd: format!("{}/proj", home),
                model: "sonnet".to_string(),
                input_tokens: 10,
                output_tokens: 10,
                cache_creation_tokens: 0,
                cache_read_tokens: 0,
                tool_names: vec![],
                git_branch: String::new(),
                message_type: MessageType::Assistant,
                uuid: String::new(),
                parent_uuid: String::new(),
                text_length: 0,
                text_word_count: 0,
                tool_use_ids: vec![],
                is_tool_error: None,
            },
        ];
        state.ingest(&records, 0);

        assert_eq!(state.active_session_count(&Settings::default()), 1);
    }

    // ── New Phase 2 tests ────────────────────────────────

    fn make_record_with_tools(
        session: &str,
        model: &str,
        input: u64,
        output: u64,
        tools: Vec<&str>,
        branch: &str,
    ) -> MessageRecord {
        let home = dirs::home_dir().unwrap().to_string_lossy().to_string();
        MessageRecord {
            session_id: session.to_string(),
            timestamp: Utc::now(),
            cwd: format!("{}/test-project", home),
            model: model.to_string(),
            input_tokens: input,
            output_tokens: output,
            cache_creation_tokens: 0,
            cache_read_tokens: 0,
            tool_names: tools.into_iter().map(String::from).collect(),
            git_branch: branch.to_string(),
            message_type: MessageType::Assistant,
            uuid: String::new(),
            parent_uuid: String::new(),
            text_length: 0,
            text_word_count: 0,
            tool_use_ids: vec![],
            is_tool_error: None,
        }
    }

    #[test]
    fn test_ingest_accumulates_tools() {
        let mut state = MetricsState::default();
        let records = vec![
            make_record_with_tools("s1", "sonnet", 10, 20, vec!["Bash", "Read"], ""),
            make_record_with_tools("s1", "sonnet", 10, 20, vec!["Bash", "Write"], ""),
        ];
        state.ingest(&records, 0);

        assert_eq!(state.tools["Bash"], 2);
        assert_eq!(state.tools["Read"], 1);
        assert_eq!(state.tools["Write"], 1);
    }

    #[test]
    fn test_tools_sorted_order() {
        let mut state = MetricsState::default();
        let records = vec![
            make_record_with_tools("s1", "sonnet", 10, 20, vec!["Bash", "Read", "Bash"], ""),
            make_record_with_tools("s1", "sonnet", 10, 20, vec!["Write"], ""),
        ];
        state.ingest(&records, 0);

        let sorted = state.tools_sorted();
        assert_eq!(*sorted[0].0, "Bash");
        assert_eq!(*sorted[0].1, 2);
    }

    #[test]
    fn test_ingest_accumulates_branches() {
        let mut state = MetricsState::default();
        let records = vec![
            make_record_with_tools("s1", "sonnet", 100, 200, vec![], "main"),
            make_record_with_tools("s2", "sonnet", 300, 400, vec![], "feature/auth"),
        ];
        state.ingest(&records, 0);

        assert_eq!(state.branches.len(), 2);
        assert_eq!(state.branches["main"].input_tokens, 100);
        assert_eq!(state.branches["main"].output_tokens, 200);
        assert_eq!(state.branches["feature/auth"].input_tokens, 300);
        assert_eq!(state.branches["feature/auth"].output_tokens, 400);
    }

    #[test]
    fn test_branches_sorted_order() {
        let mut state = MetricsState::default();
        let records = vec![
            make_record_with_tools("s1", "sonnet", 100, 100, vec![], "small-branch"),
            make_record_with_tools("s2", "sonnet", 1000, 1000, vec![], "big-branch"),
        ];
        state.ingest(&records, 0);

        let sorted = state.branches_sorted();
        assert_eq!(sorted[0].name, "big-branch");
        assert_eq!(sorted[1].name, "small-branch");
    }

    #[test]
    fn test_session_branch_uses_latest() {
        let mut state = MetricsState::default();
        let records = vec![
            make_record_with_tools("s1", "sonnet", 10, 20, vec![], "old-branch"),
            make_record_with_tools("s1", "sonnet", 10, 20, vec![], "new-branch"),
        ];
        state.ingest(&records, 0);

        assert_eq!(state.sessions["s1"].branch, "new-branch");
    }

    #[test]
    fn test_empty_branch_not_tracked() {
        let mut state = MetricsState::default();
        let records = vec![make_record_with_tools("s1", "sonnet", 10, 20, vec![], "")];
        state.ingest(&records, 0);

        assert!(state.branches.is_empty());
    }

    #[test]
    fn test_burn_window_populated() {
        let mut state = MetricsState::default();
        let records = vec![make_record_with_tools("s1", "sonnet", 10, 200, vec![], "")];
        state.ingest(&records, 0);

        assert_eq!(state.burn_window.len(), 1);
        assert_eq!(state.burn_window[0].1, 200);
    }

    #[test]
    fn test_burn_rate_per_minute() {
        let mut state = MetricsState::default();
        let now = Utc::now();
        // Manually push entries into the window
        state.burn_window.push_back((now, 5000));
        state.burn_window.push_back((now, 5000));

        let rate = state.burn_rate_per_minute(&Settings::default());
        // 10000 tokens over burn_rate_window_minutes (10) = 1000 tok/min
        assert!((rate - 1000.0).abs() < 0.1);
    }

    #[test]
    fn test_burn_rate_empty_is_zero() {
        let state = MetricsState::default();
        assert_eq!(state.burn_rate_per_minute(&Settings::default()), 0.0);
    }

    #[test]
    fn test_no_tools_empty_map() {
        let mut state = MetricsState::default();
        let records = vec![
            make_record("s1", "sonnet", 10, 20),
            make_record("s2", "sonnet", 30, 40),
        ];
        state.ingest(&records, 0);

        assert!(state.tools.is_empty());
    }

    // ── Phase 1: parser widening aggregator tests ────────────

    fn make_user_prompt(session: &str, text_length: u64) -> MessageRecord {
        let home = dirs::home_dir().unwrap().to_string_lossy().to_string();
        MessageRecord {
            session_id: session.to_string(),
            timestamp: Utc::now(),
            cwd: format!("{}/test-project", home),
            model: String::new(),
            input_tokens: 0,
            output_tokens: 0,
            cache_creation_tokens: 0,
            cache_read_tokens: 0,
            tool_names: vec![],
            git_branch: String::new(),
            message_type: MessageType::UserPrompt,
            uuid: String::new(),
            parent_uuid: String::new(),
            text_length,
            text_word_count: 0,
            tool_use_ids: vec![],
            is_tool_error: None,
        }
    }

    fn make_tool_result(session: &str, is_error: bool) -> MessageRecord {
        let home = dirs::home_dir().unwrap().to_string_lossy().to_string();
        MessageRecord {
            session_id: session.to_string(),
            timestamp: Utc::now(),
            cwd: format!("{}/test-project", home),
            model: String::new(),
            input_tokens: 0,
            output_tokens: 0,
            cache_creation_tokens: 0,
            cache_read_tokens: 0,
            tool_names: vec![],
            git_branch: String::new(),
            message_type: MessageType::ToolResult,
            uuid: String::new(),
            parent_uuid: String::new(),
            text_length: 0,
            text_word_count: 0,
            tool_use_ids: vec!["toolu_test".to_string()],
            is_tool_error: Some(is_error),
        }
    }

    #[test]
    fn test_ingest_user_prompt_increments_counter() {
        let mut state = MetricsState::default();
        state.ingest(&[
            make_user_prompt("s1", 42),
            make_user_prompt("s1", 10),
        ], 0);

        let s = &state.sessions["s1"];
        assert_eq!(s.user_message_count, 2);
        assert_eq!(s.user_text_length, 52);
        // Should not affect token counts
        assert_eq!(s.input_tokens, 0);
        assert_eq!(s.output_tokens, 0);
        assert_eq!(state.total_input, 0);
        assert_eq!(state.total_output, 0);
    }

    #[test]
    fn test_ingest_tool_result_increments_counter() {
        let mut state = MetricsState::default();
        state.ingest(&[
            make_tool_result("s1", false),
            make_tool_result("s1", false),
            make_tool_result("s1", true),
        ], 0);

        let s = &state.sessions["s1"];
        assert_eq!(s.tool_result_count, 3);
        assert_eq!(s.tool_error_count, 1);
        assert_eq!(s.input_tokens, 0);
    }

    #[test]
    fn test_ingest_mixed_types_same_session() {
        let mut state = MetricsState::default();
        state.ingest(&[
            make_user_prompt("s1", 20),
            make_record("s1", "sonnet", 100, 200),
            make_tool_result("s1", false),
            make_record("s1", "sonnet", 50, 100),
            make_tool_result("s1", true),
        ], 0);

        let s = &state.sessions["s1"];
        assert_eq!(s.message_count, 5);
        assert_eq!(s.user_message_count, 1);
        assert_eq!(s.tool_result_count, 2);
        assert_eq!(s.tool_error_count, 1);
        assert_eq!(s.user_text_length, 20);
        assert_eq!(s.input_tokens, 150);
        assert_eq!(s.output_tokens, 300);
        assert_eq!(state.total_input, 150);
        assert_eq!(state.total_output, 300);
        assert_eq!(state.total_messages, 5);
    }

    #[test]
    fn test_user_prompt_does_not_affect_burn_window() {
        let mut state = MetricsState::default();
        state.ingest(&[make_user_prompt("s1", 100)], 0);

        assert!(state.burn_window.is_empty());
    }

    #[test]
    fn test_tool_result_does_not_create_model_entry() {
        let mut state = MetricsState::default();
        state.ingest(&[make_tool_result("s1", false)], 0);

        // Tool results have empty model — should not create a model entry
        // (or at least not add to token counts)
        assert!(state.models.is_empty() || state.models.values().all(|m| m.message_count == 0));
    }

    // ── Phase 2: Correlation engine tests ────────────────

    fn make_record_at(
        session: &str,
        msg_type: MessageType,
        ts: DateTime<Utc>,
        tool_names: Vec<&str>,
        tool_use_ids: Vec<&str>,
        is_error: Option<bool>,
    ) -> MessageRecord {
        let home = dirs::home_dir().unwrap().to_string_lossy().to_string();
        MessageRecord {
            session_id: session.to_string(),
            timestamp: ts,
            cwd: format!("{}/test-project", home),
            model: if msg_type == MessageType::Assistant {
                "sonnet".to_string()
            } else {
                String::new()
            },
            input_tokens: if msg_type == MessageType::Assistant { 10 } else { 0 },
            output_tokens: if msg_type == MessageType::Assistant { 20 } else { 0 },
            cache_creation_tokens: 0,
            cache_read_tokens: 0,
            tool_names: tool_names.into_iter().map(String::from).collect(),
            git_branch: String::new(),
            message_type: msg_type,
            uuid: String::new(),
            parent_uuid: String::new(),
            text_length: 0,
            text_word_count: 0,
            tool_use_ids: tool_use_ids.into_iter().map(String::from).collect(),
            is_tool_error: is_error,
        }
    }

    #[test]
    fn test_turn_count_equals_user_prompts() {
        let mut state = MetricsState::default();
        let now = Utc::now();
        state.ingest(&[
            make_record_at("s1", MessageType::UserPrompt, now, vec![], vec![], None),
            make_record_at("s1", MessageType::Assistant, now, vec![], vec![], None),
            make_record_at("s1", MessageType::UserPrompt, now, vec![], vec![], None),
            make_record_at("s1", MessageType::Assistant, now, vec![], vec![], None),
        ], 0);

        assert_eq!(state.sessions["s1"].turn_count, 2);
        assert_eq!(state.sessions["s1"].assistant_message_count, 2);
    }

    #[test]
    fn test_idle_gap_detection() {
        let mut state = MetricsState::default();
        let now = Utc::now();
        let six_min_ago = now - chrono::Duration::minutes(6);

        // First message at t-6min, second at t-0 → 6 minute gap, threshold 5
        state.ingest(&[
            make_record_at("s1", MessageType::Assistant, six_min_ago, vec![], vec![], None),
        ], 5);
        state.ingest(&[
            make_record_at("s1", MessageType::Assistant, now, vec![], vec![], None),
        ], 5);

        let s = &state.sessions["s1"];
        assert_eq!(s.idle_gap_count, 1);
        assert!(s.total_idle_secs >= 360); // ~6 minutes in seconds
    }

    #[test]
    fn test_no_idle_gap_below_threshold() {
        let mut state = MetricsState::default();
        let now = Utc::now();
        let two_min_ago = now - chrono::Duration::minutes(2);

        state.ingest(&[
            make_record_at("s1", MessageType::Assistant, two_min_ago, vec![], vec![], None),
        ], 5);
        state.ingest(&[
            make_record_at("s1", MessageType::Assistant, now, vec![], vec![], None),
        ], 5);

        assert_eq!(state.sessions["s1"].idle_gap_count, 0);
        assert_eq!(state.sessions["s1"].total_idle_secs, 0);
    }

    #[test]
    fn test_idle_gap_disabled_when_zero() {
        let mut state = MetricsState::default();
        let now = Utc::now();
        let hour_ago = now - chrono::Duration::hours(1);

        state.ingest(&[
            make_record_at("s1", MessageType::Assistant, hour_ago, vec![], vec![], None),
        ], 0);
        state.ingest(&[
            make_record_at("s1", MessageType::Assistant, now, vec![], vec![], None),
        ], 0);

        assert_eq!(state.sessions["s1"].idle_gap_count, 0);
    }

    #[test]
    fn test_tool_latency_correlation() {
        let mut state = MetricsState::default();
        let now = Utc::now();
        let later = now + chrono::Duration::milliseconds(1500);

        // Assistant sends tool_use with id "t1", tool name "Bash"
        state.ingest(&[
            make_record_at("s1", MessageType::Assistant, now, vec!["Bash"], vec!["t1"], None),
        ], 0);

        // Tool result arrives 1500ms later
        state.ingest(&[
            make_record_at("s1", MessageType::ToolResult, later, vec![], vec!["t1"], Some(false)),
        ], 0);

        assert!(state.pending_tool_uses.is_empty()); // consumed
        assert_eq!(state.tool_latencies.len(), 1);
        let stats = &state.tool_latencies["Bash"];
        assert_eq!(stats.call_count, 1);
        assert!((stats.total_ms - 1500).abs() <= 1); // allow 1ms tolerance
        assert_eq!(stats.error_count, 0);
    }

    #[test]
    fn test_tool_latency_with_error() {
        let mut state = MetricsState::default();
        let now = Utc::now();
        let later = now + chrono::Duration::milliseconds(500);

        state.ingest(&[
            make_record_at("s1", MessageType::Assistant, now, vec!["Read"], vec!["t2"], None),
        ], 0);
        state.ingest(&[
            make_record_at("s1", MessageType::ToolResult, later, vec![], vec!["t2"], Some(true)),
        ], 0);

        let stats = &state.tool_latencies["Read"];
        assert_eq!(stats.call_count, 1);
        assert_eq!(stats.error_count, 1);
        assert!((stats.error_rate() - 1.0).abs() < 0.001);
    }

    #[test]
    fn test_tool_latency_multiple_calls() {
        let mut state = MetricsState::default();
        let now = Utc::now();

        // Two Bash calls with different latencies
        state.ingest(&[
            make_record_at("s1", MessageType::Assistant, now, vec!["Bash"], vec!["t1"], None),
        ], 0);
        state.ingest(&[
            make_record_at("s1", MessageType::ToolResult, now + chrono::Duration::milliseconds(1000), vec![], vec!["t1"], Some(false)),
        ], 0);
        state.ingest(&[
            make_record_at("s1", MessageType::Assistant, now + chrono::Duration::seconds(2), vec!["Bash"], vec!["t2"], None),
        ], 0);
        state.ingest(&[
            make_record_at("s1", MessageType::ToolResult, now + chrono::Duration::milliseconds(3000), vec![], vec!["t2"], Some(false)),
        ], 0);

        let stats = &state.tool_latencies["Bash"];
        assert_eq!(stats.call_count, 2);
        assert!((stats.avg_ms() - 1000.0).abs() < 5.0); // avg of 1000ms and 1000ms
        assert_eq!(stats.min_ms, Some(1000));
        assert_eq!(stats.max_ms, Some(1000));
    }

    #[test]
    fn test_unmatched_tool_result_ignored() {
        let mut state = MetricsState::default();
        let now = Utc::now();

        // Tool result with no preceding tool_use → no latency recorded
        state.ingest(&[
            make_record_at("s1", MessageType::ToolResult, now, vec![], vec!["orphan"], Some(false)),
        ], 0);

        assert!(state.tool_latencies.is_empty());
    }
}
