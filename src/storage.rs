use std::path::{Path, PathBuf};

use chrono::NaiveDate;
use rusqlite::{params, Connection};

use crate::config;
use crate::parser;
use crate::types::{MessageRecord, MessageType, MetricsState, ProjectMetrics, SessionMetrics};

const SCHEMA_VERSION: u32 = 4;

/// A row from the daily_metrics table.
#[derive(Debug, Clone)]
#[cfg_attr(not(test), allow(dead_code))]
pub struct DailyRow {
    pub date: String,
    pub project: String,
    pub model: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_tokens: u64,
    pub cache_read_tokens: u64,
    pub message_count: u64,
    pub session_count: u64,
    pub tool_counts: String,
}

impl DailyRow {
    #[allow(dead_code)]
    pub fn total_tokens(&self) -> u64 {
        self.input_tokens + self.output_tokens + self.cache_creation_tokens + self.cache_read_tokens
    }
}

/// SQLite storage layer.
pub struct Storage {
    conn: Connection,
}

impl Storage {
    /// Open (or create) the database at the default path.
    pub fn open_default() -> rusqlite::Result<Self> {
        let path = db_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        Self::open(&path)
    }

    /// Open (or create) the database at a given path.
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        let conn = Connection::open(path)?;
        // WAL mode improves concurrent read/write performance
        conn.pragma_update(None, "journal_mode", "WAL")?;
        let mut storage = Self { conn };
        storage.migrate()?;
        Ok(storage)
    }

    /// Open an in-memory database (for tests).
    #[cfg(test)]
    pub fn open_memory() -> rusqlite::Result<Self> {
        let conn = Connection::open_in_memory()?;
        let mut storage = Self { conn };
        storage.migrate()?;
        Ok(storage)
    }

    fn migrate(&mut self) -> rusqlite::Result<()> {
        let version: u32 = self
            .conn
            .pragma_query_value(None, "user_version", |row| row.get(0))?;

        if version < 1 {
            self.conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS daily_metrics (
                    date         TEXT NOT NULL,
                    project      TEXT NOT NULL,
                    model        TEXT NOT NULL,
                    input_tokens INTEGER NOT NULL DEFAULT 0,
                    output_tokens INTEGER NOT NULL DEFAULT 0,
                    cache_creation_tokens INTEGER NOT NULL DEFAULT 0,
                    cache_read_tokens INTEGER NOT NULL DEFAULT 0,
                    message_count INTEGER NOT NULL DEFAULT 0,
                    session_count INTEGER NOT NULL DEFAULT 0,
                    tool_counts  TEXT,
                    PRIMARY KEY (date, project, model)
                );

                CREATE TABLE IF NOT EXISTS sessions (
                    session_id   TEXT PRIMARY KEY,
                    date         TEXT NOT NULL,
                    project      TEXT NOT NULL,
                    model        TEXT NOT NULL,
                    branch       TEXT,
                    first_seen   TEXT NOT NULL,
                    last_seen    TEXT NOT NULL,
                    input_tokens INTEGER NOT NULL DEFAULT 0,
                    output_tokens INTEGER NOT NULL DEFAULT 0,
                    cache_creation_tokens INTEGER NOT NULL DEFAULT 0,
                    cache_read_tokens INTEGER NOT NULL DEFAULT 0,
                    message_count INTEGER NOT NULL DEFAULT 0,
                    tool_counts  TEXT
                );

                CREATE INDEX IF NOT EXISTS idx_daily_date ON daily_metrics(date);
                CREATE INDEX IF NOT EXISTS idx_sessions_date ON sessions(date);",
            )?;
        }

        if version < 2 {
            self.conn.execute_batch(
                "ALTER TABLE sessions ADD COLUMN user_message_count INTEGER NOT NULL DEFAULT 0;
                 ALTER TABLE sessions ADD COLUMN tool_result_count INTEGER NOT NULL DEFAULT 0;
                 ALTER TABLE sessions ADD COLUMN tool_error_count INTEGER NOT NULL DEFAULT 0;
                 ALTER TABLE sessions ADD COLUMN assistant_text_length INTEGER NOT NULL DEFAULT 0;
                 ALTER TABLE sessions ADD COLUMN user_text_length INTEGER NOT NULL DEFAULT 0;
                 ALTER TABLE sessions ADD COLUMN assistant_message_count INTEGER NOT NULL DEFAULT 0;
                 ALTER TABLE sessions ADD COLUMN turn_count INTEGER NOT NULL DEFAULT 0;
                 ALTER TABLE sessions ADD COLUMN idle_gap_count INTEGER NOT NULL DEFAULT 0;
                 ALTER TABLE sessions ADD COLUMN total_idle_secs INTEGER NOT NULL DEFAULT 0;
                 ALTER TABLE sessions ADD COLUMN assistant_word_count INTEGER NOT NULL DEFAULT 0;
                 ALTER TABLE sessions ADD COLUMN user_word_count INTEGER NOT NULL DEFAULT 0;",
            )?;
        }

        if version < 3 {
            self.conn.execute_batch(
                "ALTER TABLE sessions ADD COLUMN bash_command_counts TEXT DEFAULT '{}';
                 ALTER TABLE sessions ADD COLUMN file_paths_touched INTEGER DEFAULT 0;
                 ALTER TABLE sessions ADD COLUMN file_types TEXT DEFAULT '{}';
                 ALTER TABLE sessions ADD COLUMN search_before_act_ratio REAL DEFAULT 0.0;
                 ALTER TABLE sessions ADD COLUMN edit_precision_avg REAL DEFAULT 0.0;
                 ALTER TABLE sessions ADD COLUMN exploration_breadth INTEGER DEFAULT 0;
                 ALTER TABLE sessions ADD COLUMN burst_count INTEGER DEFAULT 0;
                 ALTER TABLE sessions ADD COLUMN retry_count INTEGER DEFAULT 0;
                 ALTER TABLE sessions ADD COLUMN phase_sequence TEXT DEFAULT '';
                 ALTER TABLE sessions ADD COLUMN conversation_depth INTEGER DEFAULT 0;
                 ALTER TABLE sessions ADD COLUMN conversation_branch_count INTEGER DEFAULT 0;
                 ALTER TABLE sessions ADD COLUMN compaction_count INTEGER DEFAULT 0;
                 ALTER TABLE sessions ADD COLUMN cache_efficiency REAL DEFAULT 0.0;
                 ALTER TABLE sessions ADD COLUMN subagent_count INTEGER DEFAULT 0;
                 ALTER TABLE sessions ADD COLUMN tool_sequence_count INTEGER DEFAULT 0;

                 CREATE TABLE IF NOT EXISTS daily_tool_details (
                     date TEXT NOT NULL,
                     tool_name TEXT NOT NULL,
                     call_count INTEGER DEFAULT 0,
                     error_count INTEGER DEFAULT 0,
                     avg_latency_ms REAL DEFAULT 0.0,
                     total_cost_estimate REAL DEFAULT 0.0,
                     PRIMARY KEY (date, tool_name)
                 );

                 CREATE TABLE IF NOT EXISTS daily_file_activity (
                     date TEXT NOT NULL,
                     file_path TEXT NOT NULL,
                     read_count INTEGER DEFAULT 0,
                     write_count INTEGER DEFAULT 0,
                     edit_count INTEGER DEFAULT 0,
                     grep_count INTEGER DEFAULT 0,
                     PRIMARY KEY (date, file_path)
                 );

                 CREATE TABLE IF NOT EXISTS daily_bash_commands (
                     date TEXT NOT NULL,
                     category TEXT NOT NULL,
                     count INTEGER DEFAULT 0,
                     error_count INTEGER DEFAULT 0,
                     PRIMARY KEY (date, category)
                 );",
            )?;
        }

        if version < 4 {
            self.conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS inferred_metric_versions (
                    date       TEXT NOT NULL,
                    metric_id  TEXT NOT NULL,
                    version    INTEGER NOT NULL,
                    description TEXT,
                    PRIMARY KEY (date, metric_id)
                );",
            )?;
        }

        self.conn
            .pragma_update(None, "user_version", SCHEMA_VERSION)?;
        Ok(())
    }

    /// Upsert a batch of records into daily_metrics (grouped by date+project+model).
    pub fn upsert_daily(&self, records: &[MessageRecord]) -> rusqlite::Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        Self::upsert_daily_tx(&tx, records)?;
        tx.commit()
    }

    /// Upsert daily_metrics within an existing transaction.
    fn upsert_daily_tx(
        tx: &rusqlite::Transaction<'_>,
        records: &[MessageRecord],
    ) -> rusqlite::Result<()> {
        let mut stmt = tx.prepare_cached(
            "INSERT INTO daily_metrics (date, project, model, input_tokens, output_tokens,
             cache_creation_tokens, cache_read_tokens, message_count, session_count, tool_counts)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 0, ?9)
             ON CONFLICT(date, project, model) DO UPDATE SET
               input_tokens = input_tokens + excluded.input_tokens,
               output_tokens = output_tokens + excluded.output_tokens,
               cache_creation_tokens = cache_creation_tokens + excluded.cache_creation_tokens,
               cache_read_tokens = cache_read_tokens + excluded.cache_read_tokens,
               message_count = message_count + excluded.message_count",
        )?;

        for rec in records {
            let date = rec.timestamp.format("%Y-%m-%d").to_string();
            let project = parser::short_project_name(&rec.cwd);
            let tool_json = Self::build_tool_json(&rec.tool_names);

            stmt.execute(params![
                date,
                project,
                &rec.model,
                rec.input_tokens,
                rec.output_tokens,
                rec.cache_creation_tokens,
                rec.cache_read_tokens,
                1u64, // message_count increment
                tool_json,
            ])?;
        }
        Ok(())
    }

    /// Upsert session-level rows from a batch of records.
    pub fn upsert_sessions(&self, records: &[MessageRecord]) -> rusqlite::Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        Self::upsert_sessions_tx(&tx, records)?;
        tx.commit()
    }

    /// Upsert sessions within an existing transaction.
    fn upsert_sessions_tx(
        tx: &rusqlite::Transaction<'_>,
        records: &[MessageRecord],
    ) -> rusqlite::Result<()> {
        let mut stmt = tx.prepare_cached(
            "INSERT INTO sessions (session_id, date, project, model, branch,
             first_seen, last_seen, input_tokens, output_tokens,
             cache_creation_tokens, cache_read_tokens, message_count, tool_counts,
             user_message_count, tool_result_count, tool_error_count,
             assistant_text_length, user_text_length, assistant_message_count,
             turn_count, idle_gap_count, total_idle_secs,
             assistant_word_count, user_word_count)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
                     ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24)
             ON CONFLICT(session_id) DO UPDATE SET
               model = CASE WHEN excluded.model != 'unknown' AND excluded.model != ''
                            THEN excluded.model ELSE sessions.model END,
               branch = CASE WHEN excluded.branch != '' THEN excluded.branch ELSE sessions.branch END,
               last_seen = MAX(sessions.last_seen, excluded.last_seen),
               input_tokens = sessions.input_tokens + excluded.input_tokens,
               output_tokens = sessions.output_tokens + excluded.output_tokens,
               cache_creation_tokens = sessions.cache_creation_tokens + excluded.cache_creation_tokens,
               cache_read_tokens = sessions.cache_read_tokens + excluded.cache_read_tokens,
               message_count = sessions.message_count + excluded.message_count,
               user_message_count = sessions.user_message_count + excluded.user_message_count,
               tool_result_count = sessions.tool_result_count + excluded.tool_result_count,
               tool_error_count = sessions.tool_error_count + excluded.tool_error_count,
               assistant_text_length = sessions.assistant_text_length + excluded.assistant_text_length,
               user_text_length = sessions.user_text_length + excluded.user_text_length,
               assistant_message_count = sessions.assistant_message_count + excluded.assistant_message_count,
               turn_count = sessions.turn_count + excluded.turn_count,
               idle_gap_count = sessions.idle_gap_count + excluded.idle_gap_count,
               total_idle_secs = sessions.total_idle_secs + excluded.total_idle_secs,
               assistant_word_count = sessions.assistant_word_count + excluded.assistant_word_count,
               user_word_count = sessions.user_word_count + excluded.user_word_count",
        )?;

        for rec in records {
            let date = rec.timestamp.format("%Y-%m-%d").to_string();
            let project = parser::short_project_name(&rec.cwd);
            let ts_str = rec.timestamp.to_rfc3339();
            let tool_json = Self::build_tool_json(&rec.tool_names);

            let is_user = rec.message_type == MessageType::UserPrompt;
            let is_tool = rec.message_type == MessageType::ToolResult;
            let is_assistant = rec.message_type == MessageType::Assistant;
            let is_error = rec.is_tool_error == Some(true);

            stmt.execute(params![
                &rec.session_id,
                date,
                project,
                &rec.model,
                &rec.git_branch,
                &ts_str,
                &ts_str,
                rec.input_tokens,
                rec.output_tokens,
                rec.cache_creation_tokens,
                rec.cache_read_tokens,
                1u64,
                tool_json,
                if is_user { 1u64 } else { 0 },
                if is_tool { 1u64 } else { 0 },
                if is_tool && is_error { 1u64 } else { 0 },
                if is_assistant { rec.text_length } else { 0 },
                if is_user { rec.text_length } else { 0 },
                if is_assistant { 1u64 } else { 0 },
                if is_user { 1u64 } else { 0 }, // turn_count
                0u64, // idle_gap_count (computed at ingest time, not per-record)
                0i64, // total_idle_secs
                if is_assistant { rec.text_word_count } else { 0 },
                if is_user { rec.text_word_count } else { 0 },
            ])?;
        }
        Ok(())
    }

    /// Build a JSON string of tool name counts, used by both upsert methods.
    fn build_tool_json(tool_names: &[String]) -> String {
        if tool_names.is_empty() {
            "{}".to_string()
        } else {
            let map: std::collections::HashMap<&str, u32> = tool_names
                .iter()
                .fold(std::collections::HashMap::new(), |mut m, t| {
                    *m.entry(t.as_str()).or_insert(0) += 1;
                    m
                });
            serde_json::to_string(&map).unwrap_or_else(|_| "{}".to_string())
        }
    }

    /// Persist records: upsert both daily_metrics and sessions.
    pub fn persist(&self, records: &[MessageRecord]) -> rusqlite::Result<()> {
        self.upsert_daily(records)?;
        self.upsert_sessions(records)?;
        Ok(())
    }

    /// Clear all data for the given dates, then insert fresh records.
    /// This avoids double-counting when the same JSONL data is re-read on restart.
    /// Delete + insert are wrapped in a single transaction to prevent data loss
    /// if the process crashes mid-rebuild.
    pub fn rebuild_from_records(&self, records: &[MessageRecord]) -> rusqlite::Result<()> {
        if records.is_empty() {
            return Ok(());
        }

        // Collect unique dates being rebuilt
        let dates: std::collections::HashSet<String> = records
            .iter()
            .map(|r| r.timestamp.format("%Y-%m-%d").to_string())
            .collect();

        let tx = self.conn.unchecked_transaction()?;

        // Clear existing data for these dates
        for date in &dates {
            tx.execute("DELETE FROM daily_metrics WHERE date = ?1", params![date])?;
            tx.execute("DELETE FROM sessions WHERE date = ?1", params![date])?;
        }

        // Insert fresh within the same transaction (additive upsert is safe since we cleared first)
        Self::upsert_daily_tx(&tx, records)?;
        Self::upsert_sessions_tx(&tx, records)?;

        tx.commit()?;
        Ok(())
    }

    /// Load today's data into a MetricsState (for fast startup).
    pub fn load_today(&self) -> rusqlite::Result<Option<MetricsState>> {
        let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
        self.load_date(&today)
    }

    /// Load a single day's data into a MetricsState.
    fn load_date(&self, date: &str) -> rusqlite::Result<Option<MetricsState>> {
        let mut state = MetricsState::default();
        let mut found = false;

        // Load daily_metrics
        let mut stmt = self.conn.prepare(
            "SELECT project, model, input_tokens, output_tokens,
                    cache_creation_tokens, cache_read_tokens, message_count, session_count
             FROM daily_metrics WHERE date = ?1",
        )?;
        let mut rows = stmt.query(params![date])?;
        while let Some(row) = rows.next()? {
            found = true;
            let project: String = row.get(0)?;
            let model: String = row.get(1)?;
            let input: u64 = row.get(2)?;
            let output: u64 = row.get(3)?;
            let cache_creation: u64 = row.get(4)?;
            let cache_read: u64 = row.get(5)?;
            let msg_count: u64 = row.get(6)?;

            state.total_input += input;
            state.total_output += output;
            state.total_cache_creation += cache_creation;
            state.total_cache_read += cache_read;
            state.total_messages += msg_count;

            let pm = state
                .projects
                .entry(project.clone())
                .or_insert_with(|| ProjectMetrics {
                    name: project,
                    ..Default::default()
                });
            pm.input_tokens += input;
            pm.output_tokens += output;
            pm.cache_creation_tokens += cache_creation;
            pm.cache_read_tokens += cache_read;

            let mm = state.models.entry(model).or_default();
            mm.input_tokens += input;
            mm.output_tokens += output;
            mm.cache_creation_tokens += cache_creation;
            mm.cache_read_tokens += cache_read;
            mm.message_count += msg_count;
        }

        // Load sessions for this date
        let mut stmt = self.conn.prepare(
            "SELECT session_id, project, model, branch, first_seen, last_seen,
                    input_tokens, output_tokens, cache_creation_tokens, cache_read_tokens,
                    message_count,
                    user_message_count, tool_result_count, tool_error_count,
                    assistant_text_length, user_text_length, assistant_message_count,
                    turn_count, idle_gap_count, total_idle_secs,
                    assistant_word_count, user_word_count
             FROM sessions WHERE date = ?1",
        )?;
        let mut rows = stmt.query(params![date])?;
        while let Some(row) = rows.next()? {
            found = true;
            let session_id: String = row.get(0)?;
            let project: String = row.get(1)?;
            let model: String = row.get(2)?;
            let branch: String = row.get::<_, Option<String>>(3)?.unwrap_or_default();
            let first_seen: String = row.get(4)?;
            let last_seen: String = row.get(5)?;
            let input: u64 = row.get(6)?;
            let output: u64 = row.get(7)?;
            let cache_creation: u64 = row.get(8)?;
            let cache_read: u64 = row.get(9)?;
            let msg_count: u64 = row.get(10)?;

            let first_ts = first_seen
                .parse()
                .unwrap_or_else(|_| chrono::Utc::now());
            let last_ts = last_seen
                .parse()
                .unwrap_or_else(|_| chrono::Utc::now());

            // Update project session_count
            if let Some(pm) = state.projects.get_mut(&project) {
                pm.session_count += 1;
            }

            if let Some(ts) = state.last_updated {
                if last_ts > ts {
                    state.last_updated = Some(last_ts);
                }
            } else {
                state.last_updated = Some(last_ts);
            }

            state.sessions.insert(
                session_id,
                SessionMetrics {
                    project,
                    model,
                    first_seen: first_ts,
                    last_seen: last_ts,
                    input_tokens: input,
                    output_tokens: output,
                    cache_creation_tokens: cache_creation,
                    cache_read_tokens: cache_read,
                    message_count: msg_count,
                    branch,
                    user_message_count: row.get(11)?,
                    tool_result_count: row.get(12)?,
                    tool_error_count: row.get(13)?,
                    assistant_text_length: row.get(14)?,
                    user_text_length: row.get(15)?,
                    assistant_message_count: row.get(16)?,
                    turn_count: row.get(17)?,
                    idle_gap_count: row.get(18)?,
                    total_idle_secs: row.get(19)?,
                    assistant_word_count: row.get(20)?,
                    user_word_count: row.get(21)?,
                },
            );
        }

        if !found {
            return Ok(None);
        }

        Ok(Some(state))
    }

    /// Query daily_metrics for a date range, returning aggregated rows per day.
    #[cfg(test)]
    pub fn query_daily_range(&self, start: &str, end: &str) -> rusqlite::Result<Vec<DailyRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT date, project, model, input_tokens, output_tokens,
                    cache_creation_tokens, cache_read_tokens, message_count, session_count,
                    COALESCE(tool_counts, '{}')
             FROM daily_metrics WHERE date >= ?1 AND date <= ?2
             ORDER BY date",
        )?;
        let rows = stmt.query_map(params![start, end], |row| {
            Ok(DailyRow {
                date: row.get(0)?,
                project: row.get(1)?,
                model: row.get(2)?,
                input_tokens: row.get(3)?,
                output_tokens: row.get(4)?,
                cache_creation_tokens: row.get(5)?,
                cache_read_tokens: row.get(6)?,
                message_count: row.get(7)?,
                session_count: row.get(8)?,
                tool_counts: row.get(9)?,
            })
        })?;
        rows.collect()
    }

    /// Aggregate daily totals for a date range (one entry per day, summed across projects/models).
    pub fn daily_totals(&self, start: &str, end: &str) -> rusqlite::Result<Vec<(String, u64, f64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT date,
                    SUM(input_tokens + output_tokens + cache_creation_tokens + cache_read_tokens),
                    SUM(message_count)
             FROM daily_metrics WHERE date >= ?1 AND date <= ?2
             GROUP BY date ORDER BY date",
        )?;
        let rows = stmt.query_map(params![start, end], |row| {
            let date: String = row.get(0)?;
            let total_tokens: u64 = row.get(1)?;
            let messages: f64 = row.get::<_, f64>(2)?;
            Ok((date, total_tokens, messages))
        })?;
        rows.collect()
    }

    /// Per-project daily totals for sparklines.
    pub fn project_daily_totals(
        &self,
        project: &str,
        start: &str,
        end: &str,
    ) -> rusqlite::Result<Vec<(String, u64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT date,
                    SUM(input_tokens + output_tokens + cache_creation_tokens + cache_read_tokens)
             FROM daily_metrics WHERE project = ?1 AND date >= ?2 AND date <= ?3
             GROUP BY date ORDER BY date",
        )?;
        let rows = stmt.query_map(params![project, start, end], |row| {
            let date: String = row.get(0)?;
            let total: u64 = row.get(1)?;
            Ok((date, total))
        })?;
        rows.collect()
    }

    /// Count total rows in daily_metrics for a date.
    pub fn has_data_for_date(&self, date: &str) -> rusqlite::Result<bool> {
        let count: u64 = self.conn.query_row(
            "SELECT COUNT(*) FROM daily_metrics WHERE date = ?1",
            params![date],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    /// Get the earliest and latest dates in the database.
    pub fn date_range(&self) -> rusqlite::Result<Option<(String, String)>> {
        let result: Option<(String, String)> = self
            .conn
            .query_row(
                "SELECT MIN(date), MAX(date) FROM daily_metrics",
                [],
                |row| {
                    let min: Option<String> = row.get(0)?;
                    let max: Option<String> = row.get(1)?;
                    Ok(min.zip(max))
                },
            )?;
        Ok(result)
    }

    /// Persist detail tables (tool details, file activity, bash commands) for a date.
    pub fn persist_details(&self, date: &str, state: &MetricsState) -> rusqlite::Result<()> {
        let tx = self.conn.unchecked_transaction()?;

        // Daily tool details
        {
            let mut stmt = tx.prepare_cached(
                "INSERT INTO daily_tool_details (date, tool_name, call_count, error_count, avg_latency_ms, total_cost_estimate)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(date, tool_name) DO UPDATE SET
                   call_count = excluded.call_count,
                   error_count = excluded.error_count,
                   avg_latency_ms = excluded.avg_latency_ms,
                   total_cost_estimate = excluded.total_cost_estimate",
            )?;
            for (tool_name, count) in &state.tools {
                let latency = state.tool_latencies.get(tool_name);
                let error_count = latency.map_or(0, |l| l.error_count);
                let avg_latency = latency.map_or(0.0, |l| l.avg_ms());
                let cost = state.cost_intel.cost_per_tool.get(tool_name).copied().unwrap_or(0.0);
                stmt.execute(params![date, tool_name, count, error_count, avg_latency, cost])?;
            }
        }

        // Daily file activity
        {
            let mut stmt = tx.prepare_cached(
                "INSERT INTO daily_file_activity (date, file_path, read_count, write_count, edit_count, grep_count)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(date, file_path) DO UPDATE SET
                   read_count = excluded.read_count,
                   write_count = excluded.write_count,
                   edit_count = excluded.edit_count,
                   grep_count = excluded.grep_count",
            )?;
            for (path, ft) in &state.file_intel.global_file_touches {
                stmt.execute(params![date, path, ft.read_count, ft.write_count, ft.edit_count, ft.grep_count])?;
            }
        }

        // Daily bash commands
        {
            let mut stmt = tx.prepare_cached(
                "INSERT INTO daily_bash_commands (date, category, count, error_count)
                 VALUES (?1, ?2, ?3, 0)
                 ON CONFLICT(date, category) DO UPDATE SET
                   count = excluded.count",
            )?;
            // Aggregate bash categories across all sessions
            let mut totals: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
            for behavior in state.session_behaviors.values() {
                for (cat, count) in &behavior.bash_categories {
                    *totals.entry(format!("{:?}", cat)).or_insert(0) += count;
                }
            }
            for (cat, count) in &totals {
                stmt.execute(params![date, cat, count])?;
            }
        }

        tx.commit()
    }

    /// Query daily tool details for a date range.
    pub fn daily_tool_details_range(
        &self,
        start: &str,
        end: &str,
    ) -> rusqlite::Result<std::collections::HashMap<String, Vec<(String, u64)>>> {
        let mut stmt = self.conn.prepare(
            "SELECT date, tool_name, call_count FROM daily_tool_details
             WHERE date >= ?1 AND date <= ?2 ORDER BY date",
        )?;
        let mut result: std::collections::HashMap<String, Vec<(String, u64)>> =
            std::collections::HashMap::new();
        let mut rows = stmt.query(params![start, end])?;
        while let Some(row) = rows.next()? {
            let date: String = row.get(0)?;
            let tool: String = row.get(1)?;
            let count: u64 = row.get(2)?;
            result.entry(tool).or_default().push((date, count));
        }
        Ok(result)
    }

    /// Query top N file activity entries.
    pub fn daily_file_activity_top(
        &self,
        start: &str,
        end: &str,
        limit: usize,
    ) -> rusqlite::Result<Vec<(String, u64, u64, u64, u64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT file_path, SUM(read_count), SUM(write_count), SUM(edit_count), SUM(grep_count)
             FROM daily_file_activity
             WHERE date >= ?1 AND date <= ?2
             GROUP BY file_path
             ORDER BY SUM(read_count + write_count + edit_count + grep_count) DESC
             LIMIT ?3",
        )?;
        let mut result = Vec::new();
        let mut rows = stmt.query(params![start, end, limit as u64])?;
        while let Some(row) = rows.next()? {
            result.push((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ));
        }
        Ok(result)
    }

    /// Query bash category counts for a date range.
    pub fn daily_bash_categories_range(
        &self,
        start: &str,
        end: &str,
    ) -> rusqlite::Result<std::collections::HashMap<String, Vec<(String, u64)>>> {
        let mut stmt = self.conn.prepare(
            "SELECT date, category, count FROM daily_bash_commands
             WHERE date >= ?1 AND date <= ?2 ORDER BY date",
        )?;
        let mut result: std::collections::HashMap<String, Vec<(String, u64)>> =
            std::collections::HashMap::new();
        let mut rows = stmt.query(params![start, end])?;
        while let Some(row) = rows.next()? {
            let date: String = row.get(0)?;
            let category: String = row.get(1)?;
            let count: u64 = row.get(2)?;
            result.entry(category).or_default().push((date, count));
        }
        Ok(result)
    }
}

impl Storage {
    /// Persist current inferred metric versions for the given date.
    pub fn persist_metric_versions(&self, date: &str) -> rusqlite::Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        {
            let mut stmt = tx.prepare_cached(
                "INSERT INTO inferred_metric_versions (date, metric_id, version, description)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(date, metric_id) DO UPDATE SET
                   version = excluded.version,
                   description = excluded.description",
            )?;
            for def in crate::metric_registry::inferred_metrics() {
                stmt.execute(params![date, def.id, def.version, def.description])?;
            }
        }
        tx.commit()
    }

    /// Query inferred metric versions recorded for a given date.
    #[allow(dead_code)]
    pub fn metric_versions_for_date(
        &self,
        date: &str,
    ) -> rusqlite::Result<Vec<(String, u32, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT metric_id, version, COALESCE(description, '')
             FROM inferred_metric_versions WHERE date = ?1",
        )?;
        let rows = stmt.query_map(params![date], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?;
        rows.collect()
    }
}

/// Default database path (platform-correct data directory).
pub fn db_path() -> PathBuf {
    dirs::data_local_dir()
        .or_else(|| dirs::config_dir())
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")).join(".config"))
        .join(config::APP_DIR_NAME)
        .join(config::DB_FILENAME)
}

/// Compute a date string N days ago.
pub fn days_ago(n: i64) -> String {
    let date = chrono::Utc::now().date_naive() - chrono::Duration::days(n);
    date.format("%Y-%m-%d").to_string()
}

/// Today's date string.
pub fn today_str() -> String {
    chrono::Utc::now().format("%Y-%m-%d").to_string()
}

/// Parse a date string to NaiveDate.
#[allow(dead_code)]
pub fn parse_date(s: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{MessageRecord, MessageType};
    use chrono::Utc;

    fn test_cwd() -> String {
        let home = dirs::home_dir().unwrap().to_string_lossy().to_string();
        format!("{}/test-project", home)
    }

    #[allow(dead_code)]
    fn make_record(session: &str, model: &str, input: u64, output: u64) -> MessageRecord {
        MessageRecord {
            session_id: session.to_string(),
            timestamp: Utc::now(),
            cwd: test_cwd(),
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

    fn make_record_on_date(
        session: &str,
        model: &str,
        input: u64,
        output: u64,
        date: &str,
    ) -> MessageRecord {
        let ts = format!("{}T12:00:00Z", date);
        MessageRecord {
            session_id: session.to_string(),
            timestamp: ts.parse().unwrap(),
            cwd: test_cwd(),
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
    fn test_open_memory() {
        let storage = Storage::open_memory().unwrap();
        assert!(storage.has_data_for_date("2026-01-01").unwrap() == false);
    }

    #[test]
    fn test_migrate_idempotent() {
        let mut storage = Storage::open_memory().unwrap();
        storage.migrate().unwrap(); // should not error on re-run
    }

    #[test]
    fn test_upsert_daily_and_query() {
        let storage = Storage::open_memory().unwrap();
        let records = vec![
            make_record_on_date("s1", "sonnet", 100, 200, "2026-03-01"),
            make_record_on_date("s1", "sonnet", 150, 250, "2026-03-01"),
        ];
        storage.upsert_daily(&records).unwrap();

        let rows = storage
            .query_daily_range("2026-03-01", "2026-03-01")
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].input_tokens, 250);
        assert_eq!(rows[0].output_tokens, 450);
        assert_eq!(rows[0].message_count, 2);
    }

    #[test]
    fn test_upsert_sessions() {
        let storage = Storage::open_memory().unwrap();
        let records = vec![
            make_record_on_date("s1", "sonnet", 100, 200, "2026-03-01"),
            make_record_on_date("s1", "sonnet", 50, 60, "2026-03-01"),
        ];
        storage.upsert_sessions(&records).unwrap();

        // Verify via load_date
        let state = storage.load_date("2026-03-01").unwrap().unwrap();
        assert_eq!(state.sessions.len(), 1);
        assert_eq!(state.sessions["s1"].input_tokens, 150);
        assert_eq!(state.sessions["s1"].output_tokens, 260);
        assert_eq!(state.sessions["s1"].message_count, 2);
    }

    #[test]
    fn test_persist_and_load() {
        let storage = Storage::open_memory().unwrap();
        let records = vec![
            make_record_on_date("s1", "sonnet", 100, 200, "2026-03-03"),
            make_record_on_date("s2", "opus", 300, 400, "2026-03-03"),
        ];
        storage.persist(&records).unwrap();

        let state = storage.load_date("2026-03-03").unwrap().unwrap();
        assert_eq!(state.sessions.len(), 2);
        assert_eq!(state.total_input, 400);
        assert_eq!(state.total_output, 600);
    }

    #[test]
    fn test_load_date_empty() {
        let storage = Storage::open_memory().unwrap();
        let state = storage.load_date("2026-01-01").unwrap();
        assert!(state.is_none());
    }

    #[test]
    fn test_has_data_for_date() {
        let storage = Storage::open_memory().unwrap();
        assert!(!storage.has_data_for_date("2026-03-01").unwrap());

        let records = vec![make_record_on_date("s1", "sonnet", 10, 20, "2026-03-01")];
        storage.upsert_daily(&records).unwrap();
        assert!(storage.has_data_for_date("2026-03-01").unwrap());
    }

    #[test]
    fn test_daily_totals_range() {
        let storage = Storage::open_memory().unwrap();
        let records = vec![
            make_record_on_date("s1", "sonnet", 100, 200, "2026-03-01"),
            make_record_on_date("s2", "opus", 300, 400, "2026-03-02"),
            make_record_on_date("s3", "sonnet", 500, 600, "2026-03-03"),
        ];
        storage.upsert_daily(&records).unwrap();

        let totals = storage.daily_totals("2026-03-01", "2026-03-03").unwrap();
        assert_eq!(totals.len(), 3);
        assert_eq!(totals[0].0, "2026-03-01");
        assert_eq!(totals[0].1, 300); // 100+200
        assert_eq!(totals[1].1, 700); // 300+400
        assert_eq!(totals[2].1, 1100); // 500+600
    }

    #[test]
    fn test_project_daily_totals() {
        let storage = Storage::open_memory().unwrap();
        let records = vec![
            make_record_on_date("s1", "sonnet", 100, 200, "2026-03-01"),
            make_record_on_date("s2", "sonnet", 50, 60, "2026-03-02"),
        ];
        storage.upsert_daily(&records).unwrap();

        let totals = storage
            .project_daily_totals("test-project", "2026-03-01", "2026-03-02")
            .unwrap();
        assert_eq!(totals.len(), 2);
    }

    #[test]
    fn test_date_range() {
        let storage = Storage::open_memory().unwrap();
        assert!(storage.date_range().unwrap().is_none());

        let records = vec![
            make_record_on_date("s1", "sonnet", 10, 20, "2026-02-15"),
            make_record_on_date("s2", "sonnet", 10, 20, "2026-03-01"),
        ];
        storage.upsert_daily(&records).unwrap();

        let (min, max) = storage.date_range().unwrap().unwrap();
        assert_eq!(min, "2026-02-15");
        assert_eq!(max, "2026-03-01");
    }

    #[test]
    fn test_upsert_daily_accumulates() {
        let storage = Storage::open_memory().unwrap();

        // First batch
        let r1 = vec![make_record_on_date("s1", "sonnet", 100, 200, "2026-03-01")];
        storage.upsert_daily(&r1).unwrap();

        // Second batch — same date/project/model
        let r2 = vec![make_record_on_date("s2", "sonnet", 50, 60, "2026-03-01")];
        storage.upsert_daily(&r2).unwrap();

        let rows = storage
            .query_daily_range("2026-03-01", "2026-03-01")
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].input_tokens, 150);
        assert_eq!(rows[0].output_tokens, 260);
    }

    #[test]
    fn test_rebuild_idempotent_no_double_counting() {
        let storage = Storage::open_memory().unwrap();
        let records = vec![
            make_record_on_date("s1", "sonnet", 100, 200, "2026-03-01"),
            make_record_on_date("s2", "opus", 300, 400, "2026-03-01"),
        ];

        // First rebuild
        storage.rebuild_from_records(&records).unwrap();
        let rows = storage
            .query_daily_range("2026-03-01", "2026-03-01")
            .unwrap();
        let total_input: u64 = rows.iter().map(|r| r.input_tokens).sum();
        assert_eq!(total_input, 400); // 100 + 300

        // Second rebuild (simulates restart) — should NOT double
        storage.rebuild_from_records(&records).unwrap();
        let rows = storage
            .query_daily_range("2026-03-01", "2026-03-01")
            .unwrap();
        let total_input: u64 = rows.iter().map(|r| r.input_tokens).sum();
        assert_eq!(total_input, 400); // still 400, not 800

        // Session counts should also not double
        let state = storage.load_date("2026-03-01").unwrap().unwrap();
        assert_eq!(state.sessions.len(), 2);
        assert_eq!(state.sessions["s1"].input_tokens, 100);
        assert_eq!(state.sessions["s2"].input_tokens, 300);
    }

    #[test]
    fn test_session_branch_update() {
        let storage = Storage::open_memory().unwrap();

        let mut rec1 = make_record_on_date("s1", "sonnet", 100, 200, "2026-03-01");
        rec1.git_branch = "main".to_string();
        storage.upsert_sessions(&[rec1]).unwrap();

        let mut rec2 = make_record_on_date("s1", "sonnet", 50, 60, "2026-03-01");
        rec2.git_branch = "feature/new".to_string();
        storage.upsert_sessions(&[rec2]).unwrap();

        let state = storage.load_date("2026-03-01").unwrap().unwrap();
        assert_eq!(state.sessions["s1"].branch, "feature/new");
    }

    #[test]
    fn test_persist_and_query_metric_versions() {
        let storage = Storage::open_memory().unwrap();

        // Persist inferred metric versions for a date
        storage.persist_metric_versions("2026-03-10").unwrap();

        let versions = storage.metric_versions_for_date("2026-03-10").unwrap();
        assert!(!versions.is_empty());

        // All inferred metrics should be present
        let inferred_count = crate::metric_registry::inferred_metrics().count();
        assert_eq!(versions.len(), inferred_count);

        // Check a specific one
        let confidence = versions.iter().find(|(id, _, _)| id == "search_act_signal");
        assert!(confidence.is_some());
        let (_, ver, desc) = confidence.unwrap();
        assert_eq!(*ver, 1);
        assert!(!desc.is_empty());
    }

    #[test]
    fn test_metric_versions_idempotent() {
        let storage = Storage::open_memory().unwrap();
        storage.persist_metric_versions("2026-03-10").unwrap();
        storage.persist_metric_versions("2026-03-10").unwrap(); // should not error
        let versions = storage.metric_versions_for_date("2026-03-10").unwrap();
        let inferred_count = crate::metric_registry::inferred_metrics().count();
        assert_eq!(versions.len(), inferred_count);
    }

    #[test]
    fn test_metric_versions_empty_date() {
        let storage = Storage::open_memory().unwrap();
        let versions = storage.metric_versions_for_date("2026-01-01").unwrap();
        assert!(versions.is_empty());
    }
}
