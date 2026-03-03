use chrono::{DateTime, Utc};
use std::collections::HashMap;

/// A parsed record from a JSONL assistant message with usage data.
#[derive(Debug, Clone)]
pub struct MessageRecord {
    pub session_id: String,
    pub timestamp: DateTime<Utc>,
    pub cwd: String,
    pub model: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_tokens: u64,
    pub cache_read_tokens: u64,
}

/// Aggregated metrics for a single session.
#[derive(Debug, Clone)]
pub struct SessionMetrics {
    pub session_id: String,
    pub project: String,
    pub model: String,
    pub first_seen: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_tokens: u64,
    pub cache_read_tokens: u64,
    pub message_count: u64,
}

impl SessionMetrics {
    pub fn total_tokens(&self) -> u64 {
        self.input_tokens + self.output_tokens + self.cache_creation_tokens + self.cache_read_tokens
    }

    pub fn duration_minutes(&self) -> i64 {
        (self.last_seen - self.first_seen).num_minutes()
    }

    pub fn is_active(&self, now: DateTime<Utc>, threshold_minutes: i64) -> bool {
        (now - self.last_seen).num_minutes() < threshold_minutes
    }
}

/// Aggregated metrics for a project (decoded path).
#[derive(Debug, Clone, Default)]
pub struct ProjectMetrics {
    pub name: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_tokens: u64,
    pub cache_read_tokens: u64,
    pub session_count: u64,
}

impl ProjectMetrics {
    pub fn total_tokens(&self) -> u64 {
        self.input_tokens + self.output_tokens + self.cache_creation_tokens + self.cache_read_tokens
    }
}

/// Per-model token breakdown.
#[derive(Debug, Clone, Default)]
pub struct ModelMetrics {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_tokens: u64,
    pub cache_read_tokens: u64,
    pub message_count: u64,
}

/// The full metrics state, updated by the aggregator.
#[derive(Debug, Clone)]
pub struct MetricsState {
    pub sessions: HashMap<String, SessionMetrics>,
    pub projects: HashMap<String, ProjectMetrics>,
    pub models: HashMap<String, ModelMetrics>,
    pub total_input: u64,
    pub total_output: u64,
    pub total_cache_creation: u64,
    pub total_cache_read: u64,
    pub total_messages: u64,
    pub last_updated: Option<DateTime<Utc>>,
}

impl Default for MetricsState {
    fn default() -> Self {
        Self {
            sessions: HashMap::new(),
            projects: HashMap::new(),
            models: HashMap::new(),
            total_input: 0,
            total_output: 0,
            total_cache_creation: 0,
            total_cache_read: 0,
            total_messages: 0,
            last_updated: None,
        }
    }
}
