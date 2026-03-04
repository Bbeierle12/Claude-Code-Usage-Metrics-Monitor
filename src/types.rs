use chrono::{DateTime, Utc};
use std::collections::{HashMap, VecDeque};

/// The type of JSONL line this record was parsed from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageType {
    /// `type: "assistant"` — has usage data, model, tool_use blocks
    Assistant,
    /// `type: "user"` without `toolUseResult` — human prompt
    UserPrompt,
    /// `type: "user"` with `toolUseResult` — tool execution result
    ToolResult,
}

/// A parsed record from a JSONL line (assistant, user prompt, or tool result).
#[derive(Debug, Clone)]
#[allow(dead_code)] // uuid, parent_uuid, tool_use_ids consumed in Phase 2
pub struct MessageRecord {
    pub session_id: String,
    pub timestamp: DateTime<Utc>,
    pub cwd: String,
    pub model: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_tokens: u64,
    pub cache_read_tokens: u64,
    pub tool_names: Vec<String>,
    pub git_branch: String,
    // ── Phase 1 additions ──
    pub message_type: MessageType,
    pub uuid: String,
    pub parent_uuid: String,
    /// Character count of text content (assistant text blocks or user prompt text).
    pub text_length: u64,
    /// Word count of text content.
    pub text_word_count: u64,
    /// tool_use block IDs (assistant) or tool_result tool_use_ids (tool result).
    pub tool_use_ids: Vec<String>,
    /// For tool_result lines: whether the tool reported an error.
    pub is_tool_error: Option<bool>,
}

/// Aggregated metrics for a single session.
#[derive(Debug, Clone)]
pub struct SessionMetrics {
    pub project: String,
    pub model: String,
    pub first_seen: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_tokens: u64,
    pub cache_read_tokens: u64,
    pub message_count: u64,
    pub branch: String,
    // ── Phase 1 additions ──
    pub user_message_count: u64,
    pub tool_result_count: u64,
    pub tool_error_count: u64,
    pub assistant_text_length: u64,
    pub user_text_length: u64,
    // ── Phase 2 additions ──
    pub assistant_message_count: u64,
    /// Number of user→assistant turns (= user_message_count, each prompt starts a turn).
    pub turn_count: u64,
    /// Number of idle gaps (consecutive messages with delta > threshold).
    pub idle_gap_count: u64,
    /// Total idle time in seconds (sum of gaps exceeding threshold).
    pub total_idle_secs: i64,
    // ── Phase 3 additions ──
    pub assistant_word_count: u64,
    pub user_word_count: u64,
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

    /// Cache efficiency: fraction of input served from cache. 0.0 if no input.
    #[allow(dead_code)]
    pub fn cache_efficiency(&self) -> f64 {
        let denom = self.cache_read_tokens + self.input_tokens;
        if denom == 0 {
            return 0.0;
        }
        self.cache_read_tokens as f64 / denom as f64
    }

    /// Average character length per assistant response. 0.0 if no assistant messages.
    #[allow(dead_code)]
    pub fn avg_response_chars(&self) -> f64 {
        if self.assistant_message_count == 0 {
            return 0.0;
        }
        self.assistant_text_length as f64 / self.assistant_message_count as f64
    }

    /// Average word count per assistant response. 0.0 if no assistant messages.
    #[allow(dead_code)]
    pub fn avg_response_words(&self) -> f64 {
        if self.assistant_message_count == 0 {
            return 0.0;
        }
        self.assistant_word_count as f64 / self.assistant_message_count as f64
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

    /// Cache efficiency: fraction of input served from cache. 0.0 if no input.
    #[allow(dead_code)]
    pub fn cache_efficiency(&self) -> f64 {
        let denom = self.cache_read_tokens + self.input_tokens;
        if denom == 0 {
            return 0.0;
        }
        self.cache_read_tokens as f64 / denom as f64
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

/// Per-branch token breakdown.
#[derive(Debug, Clone, Default)]
pub struct BranchMetrics {
    pub name: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_tokens: u64,
    pub cache_read_tokens: u64,
    pub message_count: u64,
}

impl BranchMetrics {
    pub fn total_tokens(&self) -> u64 {
        self.input_tokens + self.output_tokens + self.cache_creation_tokens + self.cache_read_tokens
    }
}

/// A pending tool_use waiting for its matching tool_result.
#[derive(Debug, Clone)]
#[allow(dead_code)] // session_id available for future per-session latency breakdown
pub struct PendingToolUse {
    pub timestamp: DateTime<Utc>,
    pub tool_name: String,
    pub session_id: String,
}

/// Accumulated latency statistics for a single tool name.
#[derive(Debug, Clone, Default)]
pub struct ToolLatencyStats {
    pub call_count: u64,
    pub total_ms: i64,
    pub error_count: u64,
    pub min_ms: Option<i64>,
    pub max_ms: Option<i64>,
}

#[allow(dead_code)] // consumed in Phase 3 UI
impl ToolLatencyStats {
    pub fn avg_ms(&self) -> f64 {
        if self.call_count == 0 {
            return 0.0;
        }
        self.total_ms as f64 / self.call_count as f64
    }

    pub fn error_rate(&self) -> f64 {
        if self.call_count == 0 {
            return 0.0;
        }
        self.error_count as f64 / self.call_count as f64
    }

    pub fn record(&mut self, latency_ms: i64, is_error: bool) {
        self.call_count += 1;
        self.total_ms += latency_ms;
        if is_error {
            self.error_count += 1;
        }
        self.min_ms = Some(self.min_ms.map_or(latency_ms, |m| m.min(latency_ms)));
        self.max_ms = Some(self.max_ms.map_or(latency_ms, |m| m.max(latency_ms)));
    }
}

/// Format a token count for display: 1234 -> "1.2K", 1234567 -> "1.2M".
pub fn format_tokens(n: u64) -> String {
    if n >= 999_950 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

/// The full metrics state, updated by the aggregator.
#[derive(Debug, Clone, Default)]
pub struct MetricsState {
    pub sessions: HashMap<String, SessionMetrics>,
    pub projects: HashMap<String, ProjectMetrics>,
    pub models: HashMap<String, ModelMetrics>,
    pub tools: HashMap<String, u64>,
    pub branches: HashMap<String, BranchMetrics>,
    pub burn_window: VecDeque<(DateTime<Utc>, u64)>,
    pub total_input: u64,
    pub total_output: u64,
    pub total_cache_creation: u64,
    pub total_cache_read: u64,
    pub total_messages: u64,
    pub last_updated: Option<DateTime<Utc>>,
    // ── Phase 2 additions ──
    /// Pending tool_use blocks waiting for matching tool_result. Keyed by tool_use_id.
    pub pending_tool_uses: HashMap<String, PendingToolUse>,
    /// Per-tool-name latency and error statistics.
    pub tool_latencies: HashMap<String, ToolLatencyStats>,
    /// Set to true when data changes; cleared by the UI after cloning.
    pub dirty: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_tokens_small() {
        assert_eq!(format_tokens(0), "0");
        assert_eq!(format_tokens(999), "999");
    }

    #[test]
    fn test_format_tokens_thousands() {
        assert_eq!(format_tokens(1_000), "1.0K");
        assert_eq!(format_tokens(1_500), "1.5K");
        assert_eq!(format_tokens(999_949), "999.9K");
        assert_eq!(format_tokens(999_999), "1.0M");
    }

    #[test]
    fn test_format_tokens_millions() {
        assert_eq!(format_tokens(1_000_000), "1.0M");
        assert_eq!(format_tokens(2_500_000), "2.5M");
    }

    #[test]
    fn test_session_duration() {
        let s = SessionMetrics {
            project: "proj".into(),
            model: "sonnet".into(),
            first_seen: "2026-03-03T10:00:00Z".parse().unwrap(),
            last_seen: "2026-03-03T11:30:00Z".parse().unwrap(),
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
        };
        assert_eq!(s.duration_minutes(), 90);
        assert_eq!(s.total_tokens(), 0);
    }

    #[test]
    fn test_cache_efficiency_session() {
        let mut s = SessionMetrics {
            project: "p".into(),
            model: "s".into(),
            first_seen: "2026-03-03T10:00:00Z".parse().unwrap(),
            last_seen: "2026-03-03T10:00:00Z".parse().unwrap(),
            input_tokens: 800,
            output_tokens: 0,
            cache_creation_tokens: 0,
            cache_read_tokens: 200,
            message_count: 1,
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
        };
        // 200 / (200 + 800) = 0.2
        assert!((s.cache_efficiency() - 0.2).abs() < 0.001);

        s.input_tokens = 0;
        s.cache_read_tokens = 0;
        assert_eq!(s.cache_efficiency(), 0.0); // zero guard
    }

    #[test]
    fn test_avg_response_metrics() {
        let s = SessionMetrics {
            project: "p".into(),
            model: "s".into(),
            first_seen: "2026-03-03T10:00:00Z".parse().unwrap(),
            last_seen: "2026-03-03T10:00:00Z".parse().unwrap(),
            input_tokens: 0,
            output_tokens: 0,
            cache_creation_tokens: 0,
            cache_read_tokens: 0,
            message_count: 5,
            branch: String::new(),
            user_message_count: 0,
            tool_result_count: 0,
            tool_error_count: 0,
            assistant_text_length: 1000,
            user_text_length: 0,
            assistant_message_count: 4,
            turn_count: 0,
            idle_gap_count: 0,
            total_idle_secs: 0,
            assistant_word_count: 200,
            user_word_count: 0,
        };
        assert!((s.avg_response_chars() - 250.0).abs() < 0.001);
        assert!((s.avg_response_words() - 50.0).abs() < 0.001);
    }

    #[test]
    fn test_avg_response_zero_messages() {
        let s = SessionMetrics {
            project: "p".into(),
            model: "s".into(),
            first_seen: "2026-03-03T10:00:00Z".parse().unwrap(),
            last_seen: "2026-03-03T10:00:00Z".parse().unwrap(),
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
        };
        assert_eq!(s.avg_response_chars(), 0.0);
        assert_eq!(s.avg_response_words(), 0.0);
    }

    #[test]
    fn test_cache_efficiency_project() {
        let p = ProjectMetrics {
            name: "proj".into(),
            input_tokens: 600,
            output_tokens: 100,
            cache_creation_tokens: 0,
            cache_read_tokens: 400,
            session_count: 1,
        };
        // 400 / (400 + 600) = 0.4
        assert!((p.cache_efficiency() - 0.4).abs() < 0.001);
    }
}
