/// Named constants for heuristic thresholds and internal limits.
/// These are not user-configurable — they are formulas and safety bounds.

// ── Idle gap bucketing (C1) ──
/// Messages within this many seconds are "rapid" pace.
pub const IDLE_GAP_RAPID_SECS: i64 = 30;
/// Messages within this many seconds are "normal" pace.
pub const IDLE_GAP_NORMAL_SECS: i64 = 120;
/// Messages within this many seconds are "thinking" pace. Beyond = "away".
pub const IDLE_GAP_THINKING_SECS: i64 = 600;

// ── Burst detection (C2) ──
/// Maximum gap (seconds) between messages to be considered part of the same burst.
pub const BURST_GAP_SECS: i64 = 5;

// ── Retry detection (C3) ──
/// Same tool+file within this many seconds counts as a retry.
pub const RETRY_WINDOW_SECS: i64 = 120;

// ── Prompt intent (C4) ──
/// Prompts shorter than this (in characters) are classified as "directives".
pub const PROMPT_DIRECTIVE_THRESHOLD: u64 = 80;

// ── Token waste (C5) ──
/// High input with low output suggests wasted context.
pub const TOKEN_WASTE_INPUT_MIN: u64 = 50_000;
pub const TOKEN_WASTE_OUTPUT_MAX: u64 = 100;

// ── Cache efficiency (C6) ──
/// Maximum number of cache efficiency samples to retain.
pub const CACHE_EFFICIENCY_SAMPLE_CAP: usize = 200;

// ── TDD sequence (C7) ──
/// Maximum length of the TDD tool sequence window.
pub const TDD_SEQUENCE_CAP: usize = 20;

// ── Tree depth (C8) ──
/// Safety limit for conversation tree depth traversal.
pub const TREE_DEPTH_LIMIT: u32 = 50;

// ── Stdout truncation (C9) ──
/// Maximum characters of stdout to retain in tool output details.
pub const STDOUT_TRUNCATION_LIMIT: usize = 500;

// ── Recent tool calls (related to C3/C7) ──
/// Maximum number of recent tool calls to retain for retry detection.
pub const RECENT_TOOL_CALLS_CAP: usize = 20;

// ── Prompt length tracking ──
/// Maximum number of prompt lengths to retain per session.
pub const PROMPT_LENGTHS_CAP: usize = 100;

// ── Detail persist interval (C10) ──
/// Seconds between periodic detail table persistence.
pub const DETAIL_PERSIST_INTERVAL_SECS: u64 = 30;

// ── Repaint interval (C11) ──
/// Seconds between UI repaints when idle.
pub const REPAINT_INTERVAL_SECS: u64 = 2;
