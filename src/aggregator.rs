use std::cmp::Reverse;

use chrono::{Duration, Timelike, Utc};

use crate::heuristics;
use crate::parser;
use crate::settings::Settings;
use crate::types::{
    BashCategory, BranchMetrics, Burst, GitSubCommand, MessageRecord, MessageType,
    MetricsState, PendingToolUse, ProjectMetrics, SessionMetrics, SessionPhase,
};

/// Classify a bash command into a category and optional git sub-command.
pub fn classify_bash(cmd: &str) -> (BashCategory, Option<GitSubCommand>) {
    let trimmed = cmd.trim();
    let first_word = trimmed.split_whitespace().next().unwrap_or("");

    // Check for rule violations first (using cat/head/tail/sed/awk when dedicated tools exist)
    match first_word {
        "cat" | "head" | "tail" | "sed" | "awk" => {
            return (BashCategory::RuleViolation, None);
        }
        _ => {}
    }

    match first_word {
        "git" | "gh" => {
            let sub = trimmed.split_whitespace().nth(1).unwrap_or("");
            let git_sub = match sub {
                "diff" => GitSubCommand::Diff,
                "status" => GitSubCommand::Status,
                "commit" => GitSubCommand::Commit,
                "push" => GitSubCommand::Push,
                "branch" | "checkout" | "switch" => GitSubCommand::Branch,
                "log" => GitSubCommand::Log,
                "add" => GitSubCommand::Add,
                _ => GitSubCommand::Other,
            };
            (BashCategory::Git, Some(git_sub))
        }
        "cargo" => {
            // Distinguish cargo sub-commands
            let sub = trimmed.split_whitespace().nth(1).unwrap_or("");
            match sub {
                "test" => (BashCategory::Test, None),
                "clippy" | "fmt" => (BashCategory::Lint, None),
                _ => (BashCategory::PackageManager, None),
            }
        }
        "npm" | "yarn" | "pnpm" | "bun" | "pip" | "pip3" | "go" | "poetry"
        | "pipenv" | "conda" => (BashCategory::PackageManager, None),
        "eslint" | "prettier" | "rustfmt" | "clippy" | "ruff" | "black" | "flake8"
        | "mypy" | "tsc" => (BashCategory::Lint, None),
        "docker" | "docker-compose" | "podman" => (BashCategory::Docker, None),
        "curl" | "wget" | "ssh" | "scp" | "rsync" | "nc" | "netcat" => {
            (BashCategory::Network, None)
        }
        _ => {
            // Check for test/lint commands in compound expressions
            if trimmed.contains("cargo test")
                || trimmed.contains("npm test")
                || trimmed.contains("yarn test")
                || trimmed.contains("pytest")
                || trimmed.contains("jest")
            {
                (BashCategory::Test, None)
            } else if trimmed.contains("cargo clippy") || trimmed.contains("cargo fmt") {
                (BashCategory::Lint, None)
            } else {
                (BashCategory::Other, None)
            }
        }
    }
}

/// Compute max depth of a conversation tree via iterative BFS. Bounded to depth 50.
fn compute_tree_depth(parent_to_children: &std::collections::HashMap<String, Vec<String>>) -> u32 {
    if parent_to_children.is_empty() {
        return 0;
    }

    let all_children: std::collections::HashSet<&String> = parent_to_children
        .values()
        .flat_map(|v| v.iter())
        .collect();
    let roots: Vec<&String> = parent_to_children
        .keys()
        .filter(|k| !all_children.contains(k))
        .collect();

    let mut max_depth: u32 = 0;
    let mut stack: Vec<(&str, u32)> = roots.iter().map(|r| (r.as_str(), 1u32)).collect();

    while let Some((node, depth)) = stack.pop() {
        if depth > heuristics::TREE_DEPTH_LIMIT {
            return 50;
        }
        max_depth = max_depth.max(depth);
        if let Some(children) = parent_to_children.get(node) {
            for child in children {
                stack.push((child.as_str(), depth + 1));
            }
        }
    }

    max_depth
}

/// Detect session phase from tool usage patterns.
fn detect_phase(tools: &[String]) -> SessionPhase {
    let has_read = tools.iter().any(|t| t == "Read" || t == "Grep" || t == "Glob");
    let has_write = tools.iter().any(|t| t == "Write" || t == "Edit");
    let has_bash = tools.iter().any(|t| t == "Bash");
    let has_agent = tools.iter().any(|t| t == "Agent");

    if has_write && has_bash {
        SessionPhase::Verify
    } else if has_write {
        SessionPhase::Implement
    } else if has_agent {
        SessionPhase::Plan
    } else if has_read {
        SessionPhase::Explore
    } else {
        SessionPhase::Unknown
    }
}

impl MetricsState {
    /// Ingest a batch of new message records (assistant, user prompt, or tool result).
    /// Does NOT prune the burn window — call `prune_burn_window()` on the main thread.
    pub fn ingest(&mut self, records: &[MessageRecord], settings: &Settings) {
        let today = Utc::now().date_naive();
        let idle_gap_minutes = settings.idle_gap_minutes;

        for rec in records {
            // Only count today's records
            if rec.timestamp.date_naive() != today {
                continue;
            }

            self.dirty = true;
            self.total_messages += 1;

            // Save previous last_updated for idle-gap computation,
            // then update to current timestamp.
            let prev_last_updated = self.last_updated;
            match self.last_updated {
                Some(prev) if rec.timestamp > prev => self.last_updated = Some(rec.timestamp),
                None => self.last_updated = Some(rec.timestamp),
                _ => {}
            }

            // ── Conversation tree building (all record types) ──
            let behavior = self
                .session_behaviors
                .entry(rec.session_id.clone())
                .or_default();

            if !rec.uuid.is_empty() && !rec.parent_uuid.is_empty() {
                behavior
                    .parent_to_children
                    .entry(rec.parent_uuid.clone())
                    .or_default()
                    .push(rec.uuid.clone());
            }

            // ── Idle gap bucketing (all record types) ──
            // Use the saved previous timestamp, not the just-updated one.
            if let Some(last_ts) = prev_last_updated {
                if last_ts < rec.timestamp {
                    let gap_secs = (rec.timestamp - last_ts).num_seconds();
                    if gap_secs > 0 {
                        if gap_secs < heuristics::IDLE_GAP_RAPID_SECS {
                            self.temporal.idle_gap_buckets.rapid += 1;
                        } else if gap_secs < heuristics::IDLE_GAP_NORMAL_SECS {
                            self.temporal.idle_gap_buckets.normal += 1;
                        } else if gap_secs < heuristics::IDLE_GAP_THINKING_SECS {
                            self.temporal.idle_gap_buckets.thinking += 1;
                        } else {
                            self.temporal.idle_gap_buckets.away += 1;
                        }
                    }

                    // ── Burst detection ──
                    if gap_secs < heuristics::BURST_GAP_SECS {
                        if let Some(last_burst) = self.temporal.bursts.last_mut() {
                            last_burst.end = rec.timestamp;
                            last_burst.message_count += 1;
                            last_burst.tool_count += rec.tool_names.len() as u64;
                        } else {
                            self.temporal.bursts.push(Burst {
                                start: last_ts,
                                end: rec.timestamp,
                                message_count: 2,
                                tool_count: rec.tool_names.len() as u64,
                            });
                        }
                    } else if gap_secs >= heuristics::BURST_GAP_SECS && !self.temporal.bursts.is_empty() {
                        // End current burst if gap too long
                        let last = self.temporal.bursts.last().unwrap();
                        if last.end == last_ts {
                            // Burst was just extended, this gap ends it
                        }
                    }
                }
            }

            // Per-session (common to all types)
            let session = self
                .sessions
                .entry(rec.session_id.clone())
                .or_insert_with(|| SessionMetrics::new(
                    parser::short_project_name(&rec.cwd),
                    rec.model.clone(),
                    rec.timestamp,
                ));

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

                    // Model usage window for plan limit tracking
                    if rec.output_tokens > 0 {
                        self.model_usage_window.push_back((
                            rec.timestamp,
                            rec.model.clone(),
                            rec.output_tokens,
                        ));
                    }

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

                    // ── Behavioral analytics for assistant messages ──
                    let behavior = self
                        .session_behaviors
                        .entry(rec.session_id.clone())
                        .or_default();

                    // Tool categorization: search vs action
                    for tool_name in &rec.tool_names {
                        match tool_name.as_str() {
                            "Read" | "Grep" | "Glob" => behavior.search_ops += 1,
                            "Write" | "Edit" | "Bash" => behavior.action_ops += 1,
                            _ => {}
                        }
                        behavior.current_turn_tools.push(tool_name.clone());
                    }

                    // File path tracking from tool inputs
                    if let Some(ref details) = rec.tool_input_details {
                        for (fp, tool) in &details.file_paths {
                            behavior.unique_files_searched.insert(fp.clone());

                            // File touch tracking
                            let ft = behavior
                                .file_touches
                                .entry(fp.clone())
                                .or_default();
                            match tool.as_str() {
                                "Read" => ft.read_count += 1,
                                "Write" => {
                                    ft.write_count += 1;
                                    behavior.recently_written_files.insert(fp.clone());
                                }
                                "Edit" => {
                                    ft.edit_count += 1;
                                    if behavior.recently_written_files.contains(fp) {
                                        behavior.write_then_edit_count += 1;
                                    }
                                }
                                "Grep" | "Glob" => ft.grep_count += 1,
                                _ => {}
                            }

                            // Global file intelligence
                            let gft = self
                                .file_intel
                                .global_file_touches
                                .entry(fp.clone())
                                .or_default();
                            match tool.as_str() {
                                "Read" => gft.read_count += 1,
                                "Write" => gft.write_count += 1,
                                "Edit" => gft.edit_count += 1,
                                "Grep" | "Glob" => gft.grep_count += 1,
                                _ => {}
                            }

                            // Extension tracking
                            if let Some(ext) = std::path::Path::new(fp)
                                .extension()
                                .and_then(|e| e.to_str())
                            {
                                *self.file_intel.extension_counts.entry(ext.to_string()).or_insert(0) += 1;
                            }

                            // Path depth
                            let depth = fp.split('/').count() as u64;
                            self.file_intel.total_path_depth += depth;
                            self.file_intel.path_count += 1;
                        }

                        // Edit precision tracking
                        for (old_len, new_len) in &details.edit_sizes {
                            behavior.total_old_len += old_len;
                            behavior.total_new_len += new_len;
                            behavior.edit_op_count += 1;
                        }

                        // Bash command classification
                        for cmd in &details.bash_commands {
                            let (category, git_sub) = classify_bash(cmd);
                            *behavior.bash_categories.entry(category).or_insert(0) += 1;
                            if let Some(sub) = git_sub {
                                *behavior.git_sub_counts.entry(sub).or_insert(0) += 1;
                            }
                            behavior.total_bash_commands += 1;

                            // TDD sequence tracking
                            if category == BashCategory::Test {
                                behavior.tdd_sequence.push_back('T');
                            }

                            // TodoWrite tracking
                            self.todo_intel.total_edit_write_bash += 1;
                        }

                        // Write/Edit also count for TDD
                        if !details.edit_sizes.is_empty() {
                            behavior.tdd_sequence.push_back('E');
                            self.todo_intel.total_edit_write_bash += 1;
                        }

                        // Retry detection: same tool+file within window
                        for (fp, tool) in &details.file_paths {
                            let key = (tool.clone(), fp.clone(), rec.timestamp);
                            let is_retry = behavior.recent_tool_calls.iter().any(|(t, f, ts)| {
                                t == tool
                                    && f == fp
                                    && (rec.timestamp - *ts).num_seconds() < heuristics::RETRY_WINDOW_SECS
                            });
                            if is_retry {
                                behavior.retry_count += 1;
                            }
                            behavior.recent_tool_calls.push_back(key);
                            if behavior.recent_tool_calls.len() > heuristics::RECENT_TOOL_CALLS_CAP {
                                behavior.recent_tool_calls.pop_front();
                            }
                        }

                        // TDD cycle detection (T-E-T)
                        if behavior.tdd_sequence.len() > heuristics::TDD_SEQUENCE_CAP {
                            behavior.tdd_sequence.pop_front();
                        }
                        if behavior.tdd_sequence.len() >= 3 {
                            let len = behavior.tdd_sequence.len();
                            if behavior.tdd_sequence[len - 3] == 'T'
                                && behavior.tdd_sequence[len - 2] == 'E'
                                && behavior.tdd_sequence[len - 1] == 'T'
                            {
                                behavior.tdd_cycle_count += 1;
                            }
                        }

                        // Subagent tracking
                        for spawn in &details.subagent_spawns {
                            behavior.subagent_count += 1;
                            if let Some(ref model) = spawn.model {
                                *behavior.subagent_models.entry(model.clone()).or_insert(0) += 1;
                            }
                        }

                        // TodoWrite tracking
                        for snapshot in &details.todo_snapshots {
                            self.todo_intel.total_todo_writes += 1;
                            self.todo_intel.todo_calls += 1;
                            if let Some(prev) = self.todo_intel.last_todo_input.get(&rec.session_id) {
                                if prev != snapshot {
                                    self.todo_intel.scope_changes += 1;
                                }
                            }
                            self.todo_intel
                                .last_todo_input
                                .insert(rec.session_id.clone(), snapshot.clone());
                        }
                    }

                    // Session phase detection
                    let phase = detect_phase(&rec.tool_names);
                    let behavior = self
                        .session_behaviors
                        .entry(rec.session_id.clone())
                        .or_default();
                    if phase != behavior.current_phase {
                        behavior.current_phase = phase;
                        behavior.phase_transitions.push((rec.timestamp, phase));
                    }

                    // Cost per tool: apportion turn cost equally among tools used
                    if !rec.tool_names.is_empty() {
                        let turn_cost = settings.estimate_cost(
                            &rec.model,
                            rec.input_tokens,
                            rec.output_tokens,
                            rec.cache_creation_tokens,
                            rec.cache_read_tokens,
                        );
                        let per_tool = turn_cost / rec.tool_names.len() as f64;
                        for tool_name in &rec.tool_names {
                            *self.cost_intel.cost_per_tool.entry(tool_name.clone()).or_insert(0.0) += per_tool;
                        }
                    }

                    // Cache efficiency sampling
                    let total_input = rec.cache_read_tokens + rec.input_tokens;
                    if total_input > 0 {
                        let eff = rec.cache_read_tokens as f64 / total_input as f64;
                        self.cost_intel.cache_efficiency_samples.push_back((rec.timestamp, eff));
                        if self.cost_intel.cache_efficiency_samples.len() > heuristics::CACHE_EFFICIENCY_SAMPLE_CAP {
                            self.cost_intel.cache_efficiency_samples.pop_front();
                        }
                    }

                    // Token waste detection (high input, low output)
                    if self.cost_intel.last_assistant_input > 0 && rec.output_tokens < heuristics::TOKEN_WASTE_OUTPUT_MAX && rec.input_tokens > heuristics::TOKEN_WASTE_INPUT_MIN {
                        self.cost_intel.token_waste_events += 1;
                        self.cost_intel.token_waste_tokens += rec.input_tokens;
                    }
                    self.cost_intel.last_assistant_input = rec.input_tokens;
                }
                MessageType::UserPrompt => {
                    session.user_message_count += 1;
                    session.user_text_length += rec.text_length;
                    session.user_word_count += rec.text_word_count;
                    session.turn_count += 1;

                    // ── Behavioral analytics for user prompts ──
                    let behavior = self
                        .session_behaviors
                        .entry(rec.session_id.clone())
                        .or_default();

                    // Flush tool sequences from previous turn
                    if !behavior.current_turn_tools.is_empty() {
                        let seq = std::mem::take(&mut behavior.current_turn_tools);
                        // Compute co-occurrence on flush
                        for i in 0..seq.len() {
                            for j in (i + 1)..seq.len() {
                                let pair = if seq[i] <= seq[j] {
                                    (seq[i].clone(), seq[j].clone())
                                } else {
                                    (seq[j].clone(), seq[i].clone())
                                };
                                *behavior.tool_cooccurrence.entry(pair).or_insert(0) += 1;
                            }
                        }
                        behavior.tool_sequences.push(seq);
                    }

                    // Prompt length tracking
                    if behavior.prompt_lengths.len() < heuristics::PROMPT_LENGTHS_CAP {
                        behavior.prompt_lengths.push(rec.text_length);
                    }

                    // Question vs directive heuristic
                    if rec.text_length < heuristics::PROMPT_DIRECTIVE_THRESHOLD {
                        behavior.directive_count += 1;
                    } else {
                        behavior.question_count += 1;
                    }

                    // Hour-of-day distribution (on first prompt per session)
                    if session.user_message_count == 1 {
                        let hour = rec.timestamp.hour() as usize;
                        self.temporal.hour_distribution[hour] += 1;
                    }
                }
                MessageType::ToolResult => {
                    session.tool_result_count += 1;
                    let is_error = rec.is_tool_error == Some(true);
                    if is_error {
                        session.tool_error_count += 1;

                        // Error retry tracking
                        let behavior = self
                            .session_behaviors
                            .entry(rec.session_id.clone())
                            .or_default();
                        behavior.error_retry_count += 1;
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

        // Update conversation tree metrics for all touched sessions
        let session_ids: Vec<String> = records
            .iter()
            .map(|r| r.session_id.clone())
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        for sid in &session_ids {
            if let Some(behavior) = self.session_behaviors.get_mut(sid) {
                behavior.max_tree_depth = compute_tree_depth(&behavior.parent_to_children);
                // Branch count = number of parents with >1 child
                behavior.branch_count = behavior
                    .parent_to_children
                    .values()
                    .filter(|v| v.len() > 1)
                    .count() as u32;
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

    /// Prune model usage window entries older than the given hours.
    pub fn prune_model_usage_window(&mut self, window_hours: f64) {
        let cutoff = Utc::now() - Duration::minutes((window_hours * 60.0) as i64);
        while self
            .model_usage_window
            .front()
            .is_some_and(|(ts, _, _)| *ts < cutoff)
        {
            self.model_usage_window.pop_front();
        }
    }

    /// Sum output tokens per model within the rolling usage window.
    /// Returns a map of model_name -> total_output_tokens.
    pub fn model_window_usage(&self, window_hours: f64) -> std::collections::HashMap<String, u64> {
        let cutoff = Utc::now() - Duration::minutes((window_hours * 60.0) as i64);
        let mut usage = std::collections::HashMap::new();
        for (ts, model, tokens) in &self.model_usage_window {
            if *ts >= cutoff {
                *usage.entry(model.clone()).or_insert(0) += tokens;
            }
        }
        usage
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

    /// Sessions sorted by last_seen descending, with their IDs.
    pub fn sessions_sorted(&self) -> Vec<(&String, &SessionMetrics)> {
        let mut sessions: Vec<_> = self.sessions.iter().collect();
        sessions.sort_by_key(|(_, s)| Reverse(s.last_seen));
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
            tool_input_details: None,
            tool_output_details: None,
        }
    }

    #[test]
    fn test_ingest_counts_tokens() {
        let mut state = MetricsState::default();
        let records = vec![
            make_record("s1", "claude-sonnet-4-5", 100, 200),
            make_record("s1", "claude-sonnet-4-5", 150, 250),
        ];
        state.ingest(&records, &Settings::default());

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
        state.ingest(&records, &Settings::default());

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
        state.ingest(&records, &Settings::default());

        assert_eq!(state.models.len(), 2);
        assert!(state.models.contains_key("claude-sonnet-4-5"));
        assert!(state.models.contains_key("claude-opus-4-5"));
    }

    #[test]
    fn test_estimated_cost_nonzero() {
        let mut state = MetricsState::default();
        let records = vec![make_record("s1", "claude-sonnet-4-5", 1_000_000, 500_000)];
        state.ingest(&records, &Settings::default());

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
        state.ingest(&records, &Settings::default());

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
            tool_input_details: None,
            tool_output_details: None,
        }];
        state.ingest(&records, &Settings::default());

        assert_eq!(state.total_input, 0);
        assert_eq!(state.total_output, 0);
        assert_eq!(state.total_messages, 0);
    }

    #[test]
    fn test_ingest_updates_last_updated() {
        let mut state = MetricsState::default();
        let records = vec![make_record("s1", "claude-sonnet-4-5", 100, 200)];
        let ts = records[0].timestamp;
        state.ingest(&records, &Settings::default());

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
            tool_input_details: None,
            tool_output_details: None,
        };
        state.ingest(&[rec1], &Settings::default());
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
            tool_input_details: None,
            tool_output_details: None,
        };
        state.ingest(&[rec2], &Settings::default());
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
                tool_input_details: None,
                tool_output_details: None,
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
                tool_input_details: None,
                tool_output_details: None,
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
                tool_input_details: None,
                tool_output_details: None,
            },
        ];
        state.ingest(&records, &Settings::default());

        let sorted = state.sessions_sorted();
        assert_eq!(sorted.len(), 3);
        // Most recent first
        assert_eq!(sorted[0].1.input_tokens, 30); // newest
        assert_eq!(sorted[1].1.input_tokens, 20); // middle
        assert_eq!(sorted[2].1.input_tokens, 10); // oldest
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
                tool_input_details: None,
                tool_output_details: None,
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
                tool_input_details: None,
                tool_output_details: None,
            },
        ];
        state.ingest(&records, &Settings::default());

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
                tool_input_details: None,
                tool_output_details: None,
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
                tool_input_details: None,
                tool_output_details: None,
            },
        ];
        state.ingest(&records, &Settings::default());

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
            tool_input_details: None,
            tool_output_details: None,
        }
    }

    #[test]
    fn test_ingest_accumulates_tools() {
        let mut state = MetricsState::default();
        let records = vec![
            make_record_with_tools("s1", "sonnet", 10, 20, vec!["Bash", "Read"], ""),
            make_record_with_tools("s1", "sonnet", 10, 20, vec!["Bash", "Write"], ""),
        ];
        state.ingest(&records, &Settings::default());

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
        state.ingest(&records, &Settings::default());

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
        state.ingest(&records, &Settings::default());

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
        state.ingest(&records, &Settings::default());

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
        state.ingest(&records, &Settings::default());

        assert_eq!(state.sessions["s1"].branch, "new-branch");
    }

    #[test]
    fn test_empty_branch_not_tracked() {
        let mut state = MetricsState::default();
        let records = vec![make_record_with_tools("s1", "sonnet", 10, 20, vec![], "")];
        state.ingest(&records, &Settings::default());

        assert!(state.branches.is_empty());
    }

    #[test]
    fn test_burn_window_populated() {
        let mut state = MetricsState::default();
        let records = vec![make_record_with_tools("s1", "sonnet", 10, 200, vec![], "")];
        state.ingest(&records, &Settings::default());

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
        state.ingest(&records, &Settings::default());

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
            tool_input_details: None,
            tool_output_details: None,
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
            tool_input_details: None,
            tool_output_details: None,
        }
    }

    #[test]
    fn test_ingest_user_prompt_increments_counter() {
        let mut state = MetricsState::default();
        state.ingest(&[
            make_user_prompt("s1", 42),
            make_user_prompt("s1", 10),
        ], &Settings::default());

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
        ], &Settings::default());

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
        ], &Settings::default());

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
        state.ingest(&[make_user_prompt("s1", 100)], &Settings::default());

        assert!(state.burn_window.is_empty());
    }

    #[test]
    fn test_tool_result_does_not_create_model_entry() {
        let mut state = MetricsState::default();
        state.ingest(&[make_tool_result("s1", false)], &Settings::default());

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
            tool_input_details: None,
            tool_output_details: None,
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
        ], &Settings::default());

        assert_eq!(state.sessions["s1"].turn_count, 2);
        assert_eq!(state.sessions["s1"].assistant_message_count, 2);
    }

    #[test]
    fn test_idle_gap_detection() {
        let mut state = MetricsState::default();
        let now = Utc::now();
        let six_min_ago = now - chrono::Duration::minutes(6);

        let mut settings = Settings::default();
        settings.idle_gap_minutes = 5;

        // First message at t-6min, second at t-0 → 6 minute gap, threshold 5
        state.ingest(&[
            make_record_at("s1", MessageType::Assistant, six_min_ago, vec![], vec![], None),
        ], &settings);
        state.ingest(&[
            make_record_at("s1", MessageType::Assistant, now, vec![], vec![], None),
        ], &settings);

        let s = &state.sessions["s1"];
        assert_eq!(s.idle_gap_count, 1);
        assert!(s.total_idle_secs >= 360); // ~6 minutes in seconds
    }

    #[test]
    fn test_no_idle_gap_below_threshold() {
        let mut state = MetricsState::default();
        let now = Utc::now();
        let two_min_ago = now - chrono::Duration::minutes(2);

        let mut settings = Settings::default();
        settings.idle_gap_minutes = 5;

        state.ingest(&[
            make_record_at("s1", MessageType::Assistant, two_min_ago, vec![], vec![], None),
        ], &settings);
        state.ingest(&[
            make_record_at("s1", MessageType::Assistant, now, vec![], vec![], None),
        ], &settings);

        assert_eq!(state.sessions["s1"].idle_gap_count, 0);
        assert_eq!(state.sessions["s1"].total_idle_secs, 0);
    }

    #[test]
    fn test_idle_gap_disabled_when_zero() {
        let mut state = MetricsState::default();
        let now = Utc::now();
        let hour_ago = now - chrono::Duration::hours(1);

        let mut settings = Settings::default();
        settings.idle_gap_minutes = 0;

        state.ingest(&[
            make_record_at("s1", MessageType::Assistant, hour_ago, vec![], vec![], None),
        ], &settings);
        state.ingest(&[
            make_record_at("s1", MessageType::Assistant, now, vec![], vec![], None),
        ], &settings);

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
        ], &Settings::default());

        // Tool result arrives 1500ms later
        state.ingest(&[
            make_record_at("s1", MessageType::ToolResult, later, vec![], vec!["t1"], Some(false)),
        ], &Settings::default());

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
        ], &Settings::default());
        state.ingest(&[
            make_record_at("s1", MessageType::ToolResult, later, vec![], vec!["t2"], Some(true)),
        ], &Settings::default());

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
        ], &Settings::default());
        state.ingest(&[
            make_record_at("s1", MessageType::ToolResult, now + chrono::Duration::milliseconds(1000), vec![], vec!["t1"], Some(false)),
        ], &Settings::default());
        state.ingest(&[
            make_record_at("s1", MessageType::Assistant, now + chrono::Duration::seconds(2), vec!["Bash"], vec!["t2"], None),
        ], &Settings::default());
        state.ingest(&[
            make_record_at("s1", MessageType::ToolResult, now + chrono::Duration::milliseconds(3000), vec![], vec!["t2"], Some(false)),
        ], &Settings::default());

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
        ], &Settings::default());

        assert!(state.tool_latencies.is_empty());
    }

    // ── classify_bash tests ──

    #[test]
    fn test_classify_bash_git() {
        let (cat, sub) = classify_bash("git status");
        assert_eq!(cat, BashCategory::Git);
        assert_eq!(sub, Some(GitSubCommand::Status));
    }

    #[test]
    fn test_classify_bash_git_commit() {
        let (cat, sub) = classify_bash("git commit -m 'message'");
        assert_eq!(cat, BashCategory::Git);
        assert_eq!(sub, Some(GitSubCommand::Commit));
    }

    #[test]
    fn test_classify_bash_npm() {
        let (cat, sub) = classify_bash("npm install lodash");
        assert_eq!(cat, BashCategory::PackageManager);
        assert!(sub.is_none());
    }

    #[test]
    fn test_classify_bash_cargo_test() {
        let (cat, _) = classify_bash("cargo test -- --nocapture");
        assert_eq!(cat, BashCategory::Test);
    }

    #[test]
    fn test_classify_bash_docker() {
        let (cat, _) = classify_bash("docker build .");
        assert_eq!(cat, BashCategory::Docker);
    }

    #[test]
    fn test_classify_bash_curl() {
        let (cat, _) = classify_bash("curl https://api.example.com");
        assert_eq!(cat, BashCategory::Network);
    }

    #[test]
    fn test_classify_bash_rule_violation() {
        let (cat, _) = classify_bash("cat /etc/passwd");
        assert_eq!(cat, BashCategory::RuleViolation);
        let (cat, _) = classify_bash("sed -i 's/foo/bar/g' file.txt");
        assert_eq!(cat, BashCategory::RuleViolation);
    }

    #[test]
    fn test_classify_bash_lint() {
        let (cat, _) = classify_bash("eslint src/");
        assert_eq!(cat, BashCategory::Lint);
    }

    #[test]
    fn test_classify_bash_other() {
        let (cat, _) = classify_bash("ls -la");
        assert_eq!(cat, BashCategory::Other);
    }

    #[test]
    fn test_classify_bash_embedded_test() {
        let (cat, _) = classify_bash("cd project && cargo test");
        assert_eq!(cat, BashCategory::Test);
    }

    // ── Behavioral analytics tests ──

    #[test]
    fn test_search_act_ratio() {
        use crate::types::SessionBehavior;
        let mut b = SessionBehavior::default();
        b.search_ops = 3;
        b.action_ops = 7;
        assert!((b.search_act_ratio() - 0.3).abs() < 0.001);
    }

    #[test]
    fn test_search_act_ratio_zero() {
        use crate::types::SessionBehavior;
        let b = SessionBehavior::default();
        assert_eq!(b.search_act_ratio(), 0.0);
    }

    #[test]
    fn test_edit_precision() {
        use crate::types::SessionBehavior;
        let mut b = SessionBehavior::default();
        b.total_old_len = 100;
        b.total_new_len = 120;
        assert!((b.edit_precision() - 1.2).abs() < 0.001);
    }

    #[test]
    fn test_edit_precision_zero_old() {
        use crate::types::SessionBehavior;
        let b = SessionBehavior::default();
        assert_eq!(b.edit_precision(), 1.0);
    }

    #[test]
    fn test_avg_prompt_length() {
        use crate::types::SessionBehavior;
        let mut b = SessionBehavior::default();
        b.prompt_lengths = vec![10, 20, 30];
        assert!((b.avg_prompt_length() - 20.0).abs() < 0.001);
    }

    #[test]
    fn test_tree_depth_computation() {
        let mut parent_to_children = std::collections::HashMap::new();
        parent_to_children.insert("a".to_string(), vec!["b".to_string()]);
        parent_to_children.insert("b".to_string(), vec!["c".to_string()]);
        parent_to_children.insert("c".to_string(), vec!["d".to_string()]);
        assert_eq!(compute_tree_depth(&parent_to_children), 4);
    }

    #[test]
    fn test_tree_depth_empty() {
        let parent_to_children = std::collections::HashMap::new();
        assert_eq!(compute_tree_depth(&parent_to_children), 0);
    }

    #[test]
    fn test_tree_depth_branching() {
        let mut parent_to_children = std::collections::HashMap::new();
        parent_to_children.insert("root".to_string(), vec!["a".to_string(), "b".to_string()]);
        parent_to_children.insert("a".to_string(), vec!["c".to_string()]);
        // root -> a -> c (depth 3), root -> b (depth 2)
        assert_eq!(compute_tree_depth(&parent_to_children), 3);
    }

    #[test]
    fn test_detect_phase_explore() {
        let tools = vec!["Read".to_string(), "Grep".to_string()];
        assert_eq!(detect_phase(&tools), SessionPhase::Explore);
    }

    #[test]
    fn test_detect_phase_implement() {
        let tools = vec!["Write".to_string()];
        assert_eq!(detect_phase(&tools), SessionPhase::Implement);
    }

    #[test]
    fn test_detect_phase_verify() {
        let tools = vec!["Edit".to_string(), "Bash".to_string()];
        assert_eq!(detect_phase(&tools), SessionPhase::Verify);
    }

    // ── Storage v3 migration test ──

    #[test]
    fn test_v3_migration_creates_tables() {
        let storage = crate::storage::Storage::open_memory().unwrap();
        // Tables should exist after migration
        assert!(storage.daily_tool_details_range("2026-01-01", "2026-12-31").is_ok());
        assert!(storage.daily_file_activity_top("2026-01-01", "2026-12-31", 10).is_ok());
        assert!(storage.daily_bash_categories_range("2026-01-01", "2026-12-31").is_ok());
    }

    #[test]
    fn test_persist_details_and_query() {
        let storage = crate::storage::Storage::open_memory().unwrap();
        let mut state = MetricsState::default();

        // Add some tool data
        state.tools.insert("Bash".to_string(), 5);
        state.tools.insert("Read".to_string(), 10);
        state.cost_intel.cost_per_tool.insert("Bash".to_string(), 0.05);
        state.cost_intel.cost_per_tool.insert("Read".to_string(), 0.02);

        // Add file intel
        state.file_intel.global_file_touches.insert(
            "/src/main.rs".to_string(),
            crate::types::FileTouch { read_count: 3, write_count: 1, edit_count: 2, grep_count: 0 },
        );

        storage.persist_details("2026-03-03", &state).unwrap();

        // Query back
        let tool_trends = storage.daily_tool_details_range("2026-03-03", "2026-03-03").unwrap();
        assert!(tool_trends.contains_key("Bash"));
        assert_eq!(tool_trends["Bash"][0].1, 5);

        let files = storage.daily_file_activity_top("2026-03-03", "2026-03-03", 10).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].0, "/src/main.rs");
        assert_eq!(files[0].1, 3); // read_count
    }
}
