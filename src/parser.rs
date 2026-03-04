use crate::types::{MessageRecord, MessageType};
use chrono::{DateTime, Utc};
use serde_json::Value;

/// Try to parse a single JSONL line into a MessageRecord.
/// Accepts `type: "assistant"` (with usage data), `type: "user"` (human prompt),
/// and `type: "user"` with `toolUseResult` (tool result).
/// Returns None for other types, malformed lines, or lines missing required fields.
pub fn parse_line(line: &str) -> Option<MessageRecord> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }

    let v: Value = serde_json::from_str(line).ok()?;

    let line_type = v.get("type")?.as_str()?;

    match line_type {
        "assistant" => parse_assistant(&v),
        "user" => parse_user(&v),
        _ => None,
    }
}

/// Parse an assistant line (has usage data, model, tool_use blocks, text blocks).
fn parse_assistant(v: &Value) -> Option<MessageRecord> {
    let session_id = v.get("sessionId")?.as_str()?.to_string();
    let cwd = v
        .get("cwd")
        .and_then(|c| c.as_str())
        .unwrap_or("unknown")
        .to_string();
    let timestamp: DateTime<Utc> = v.get("timestamp")?.as_str()?.parse().ok()?;
    let git_branch = v
        .get("gitBranch")
        .and_then(|b| b.as_str())
        .unwrap_or("")
        .to_string();
    let uuid = v
        .get("uuid")
        .and_then(|u| u.as_str())
        .unwrap_or("")
        .to_string();
    let parent_uuid = v
        .get("parentUuid")
        .and_then(|u| u.as_str())
        .unwrap_or("")
        .to_string();

    let message = v.get("message")?;

    // Claude Code logs each streaming content block (thinking, text, tool_use) as a
    // separate assistant JSONL line. All lines from the same API turn carry identical
    // cumulative usage snapshots. Only the final line has stop_reason != null and
    // contains the correct accumulated totals. Skip intermediate streaming lines to
    // avoid counting usage 3-7x.
    let stop_reason = message.get("stop_reason");
    match stop_reason {
        Some(sr) if !sr.is_null() => {} // final line — process it
        _ => return None,               // streaming intermediate or missing — skip
    }

    let model = message
        .get("model")
        .and_then(|m| m.as_str())
        .unwrap_or("unknown")
        .to_string();

    let content = message.get("content").and_then(|c| c.as_array());

    // Extract tool names, tool_use IDs, and text stats
    let mut tool_names = Vec::new();
    let mut tool_use_ids = Vec::new();
    let mut text_length: u64 = 0;
    let mut text_word_count: u64 = 0;

    if let Some(arr) = content {
        for item in arr {
            match item.get("type").and_then(|t| t.as_str()) {
                Some("tool_use") => {
                    if let Some(name) = item.get("name").and_then(|n| n.as_str()) {
                        tool_names.push(name.to_string());
                    }
                    if let Some(id) = item.get("id").and_then(|i| i.as_str()) {
                        tool_use_ids.push(id.to_string());
                    }
                }
                Some("text") => {
                    if let Some(txt) = item.get("text").and_then(|t| t.as_str()) {
                        text_length += txt.len() as u64;
                        text_word_count += txt.split_whitespace().count() as u64;
                    }
                }
                _ => {}
            }
        }
    }

    let usage = message.get("usage")?;

    let input_tokens = usage
        .get("input_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let output_tokens = usage
        .get("output_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let cache_creation_tokens = usage
        .get("cache_creation_input_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let cache_read_tokens = usage
        .get("cache_read_input_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    Some(MessageRecord {
        session_id,
        timestamp,
        cwd,
        model,
        input_tokens,
        output_tokens,
        cache_creation_tokens,
        cache_read_tokens,
        tool_names,
        git_branch,
        message_type: MessageType::Assistant,
        uuid,
        parent_uuid,
        text_length,
        text_word_count,
        tool_use_ids,
        is_tool_error: None,
    })
}

/// Parse a user line — either a human prompt or a tool result.
fn parse_user(v: &Value) -> Option<MessageRecord> {
    let session_id = v.get("sessionId")?.as_str()?.to_string();
    let cwd = v
        .get("cwd")
        .and_then(|c| c.as_str())
        .unwrap_or("unknown")
        .to_string();
    let timestamp: DateTime<Utc> = v.get("timestamp")?.as_str()?.parse().ok()?;
    let git_branch = v
        .get("gitBranch")
        .and_then(|b| b.as_str())
        .unwrap_or("")
        .to_string();
    let uuid = v
        .get("uuid")
        .and_then(|u| u.as_str())
        .unwrap_or("")
        .to_string();
    let parent_uuid = v
        .get("parentUuid")
        .and_then(|u| u.as_str())
        .unwrap_or("")
        .to_string();

    let has_tool_result = v.get("toolUseResult").is_some();

    if has_tool_result {
        parse_tool_result(v, session_id, cwd, timestamp, git_branch, uuid, parent_uuid)
    } else {
        parse_user_prompt(v, session_id, cwd, timestamp, git_branch, uuid, parent_uuid)
    }
}

/// Parse a human prompt: `type: "user"` without `toolUseResult`.
fn parse_user_prompt(
    v: &Value,
    session_id: String,
    cwd: String,
    timestamp: DateTime<Utc>,
    git_branch: String,
    uuid: String,
    parent_uuid: String,
) -> Option<MessageRecord> {
    let message = v.get("message")?;
    let content = message.get("content")?;

    let (text_length, text_word_count) = extract_text_stats(content);

    Some(MessageRecord {
        session_id,
        timestamp,
        cwd,
        model: String::new(),
        input_tokens: 0,
        output_tokens: 0,
        cache_creation_tokens: 0,
        cache_read_tokens: 0,
        tool_names: Vec::new(),
        git_branch,
        message_type: MessageType::UserPrompt,
        uuid,
        parent_uuid,
        text_length,
        text_word_count,
        tool_use_ids: Vec::new(),
        is_tool_error: None,
    })
}

/// Parse a tool result: `type: "user"` with `toolUseResult`.
fn parse_tool_result(
    v: &Value,
    session_id: String,
    cwd: String,
    timestamp: DateTime<Utc>,
    git_branch: String,
    uuid: String,
    parent_uuid: String,
) -> Option<MessageRecord> {
    let message = v.get("message")?;
    let content = message.get("content")?;

    let mut tool_use_ids = Vec::new();
    let mut is_error = false;
    let mut text_length: u64 = 0;
    let mut text_word_count: u64 = 0;

    if let Some(arr) = content.as_array() {
        for item in arr {
            if item.get("type").and_then(|t| t.as_str()) == Some("tool_result") {
                if let Some(id) = item.get("tool_use_id").and_then(|i| i.as_str()) {
                    tool_use_ids.push(id.to_string());
                }
                // is_error can be true, false, or null
                if item.get("is_error").and_then(|e| e.as_bool()) == Some(true) {
                    is_error = true;
                }
                // Content stats: can be a string or a list of text blocks
                if let Some(c) = item.get("content") {
                    let (chars, words) = extract_text_stats(c);
                    text_length += chars;
                    text_word_count += words;
                }
            }
        }
    }

    Some(MessageRecord {
        session_id,
        timestamp,
        cwd,
        model: String::new(),
        input_tokens: 0,
        output_tokens: 0,
        cache_creation_tokens: 0,
        cache_read_tokens: 0,
        tool_names: Vec::new(),
        git_branch,
        message_type: MessageType::ToolResult,
        uuid,
        parent_uuid,
        text_length,
        text_word_count,
        tool_use_ids,
        is_tool_error: Some(is_error),
    })
}

/// Extract total text character length and word count from content.
/// Content can be a string or an array of `{type: "text", text: "..."}` blocks.
/// Returns (char_count, word_count).
fn extract_text_stats(content: &Value) -> (u64, u64) {
    if let Some(s) = content.as_str() {
        return (s.len() as u64, s.split_whitespace().count() as u64);
    }
    if let Some(arr) = content.as_array() {
        let mut chars: u64 = 0;
        let mut words: u64 = 0;
        for item in arr {
            if item.get("type").and_then(|t| t.as_str()) == Some("text") {
                if let Some(txt) = item.get("text").and_then(|t| t.as_str()) {
                    chars += txt.len() as u64;
                    words += txt.split_whitespace().count() as u64;
                }
            }
        }
        return (chars, words);
    }
    (0, 0)
}

/// Parse all lines from a buffer, returning only valid MessageRecords.
pub fn parse_buffer(buf: &str) -> Vec<MessageRecord> {
    buf.lines().filter_map(parse_line).collect()
}

/// Cached home directory path to avoid repeated OS calls.
fn cached_home_dir() -> &'static str {
    use std::sync::OnceLock;
    static HOME: OnceLock<String> = OnceLock::new();
    HOME.get_or_init(|| {
        dirs::home_dir()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default()
    })
}

/// Extract a short project name from a cwd path.
/// "/home/bbeierle12/Agent-Shell" -> "Agent-Shell"
/// "/home/bbeierle12" -> "~"
pub fn short_project_name(cwd: &str) -> String {
    let home = cached_home_dir();

    if cwd == home {
        return "~".to_string();
    }

    cwd.strip_prefix(home)
        .and_then(|rest| rest.strip_prefix('/'))
        .unwrap_or(cwd)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_short_project_name() {
        let home = dirs::home_dir().unwrap().to_string_lossy().to_string();
        assert_eq!(short_project_name(&home), "~");
        assert_eq!(
            short_project_name(&format!("{}/Agent-Shell", home)),
            "Agent-Shell"
        );
    }

    // ── Assistant line tests (regression) ──

    #[test]
    fn test_parse_line_non_assistant() {
        // "user" lines now parse — but a "progress" line should still return None
        let line = r#"{"type":"progress","sessionId":"abc","timestamp":"2026-03-03T00:00:00Z","cwd":"/tmp","data":{}}"#;
        assert!(parse_line(line).is_none());
    }

    #[test]
    fn test_parse_line_assistant_with_usage() {
        let line = r#"{"type":"assistant","sessionId":"abc-123","timestamp":"2026-03-03T10:30:00Z","cwd":"/home/user/proj","uuid":"u1","parentUuid":"p1","message":{"model":"claude-opus-4-6","role":"assistant","stop_reason":"end_turn","content":[],"usage":{"input_tokens":100,"output_tokens":200,"cache_creation_input_tokens":50,"cache_read_input_tokens":1000}}}"#;
        let rec = parse_line(line).unwrap();
        assert_eq!(rec.message_type, MessageType::Assistant);
        assert_eq!(rec.session_id, "abc-123");
        assert_eq!(rec.model, "claude-opus-4-6");
        assert_eq!(rec.input_tokens, 100);
        assert_eq!(rec.output_tokens, 200);
        assert_eq!(rec.cache_creation_tokens, 50);
        assert_eq!(rec.cache_read_tokens, 1000);
        assert_eq!(rec.uuid, "u1");
        assert_eq!(rec.parent_uuid, "p1");
    }

    #[test]
    fn test_parse_line_empty_string() {
        assert!(parse_line("").is_none());
    }

    #[test]
    fn test_parse_line_whitespace_only() {
        assert!(parse_line("   \n").is_none());
    }

    #[test]
    fn test_parse_line_invalid_json() {
        assert!(parse_line("{not json}").is_none());
    }

    #[test]
    fn test_parse_line_missing_usage() {
        let line = r#"{"type":"assistant","sessionId":"abc","timestamp":"2026-03-03T10:00:00Z","cwd":"/tmp","message":{"model":"sonnet","role":"assistant","content":[]}}"#;
        assert!(parse_line(line).is_none());
    }

    #[test]
    fn test_parse_line_missing_session_id() {
        let line = r#"{"type":"assistant","timestamp":"2026-03-03T10:00:00Z","cwd":"/tmp","message":{"model":"sonnet","role":"assistant","stop_reason":"end_turn","content":[],"usage":{"input_tokens":10,"output_tokens":20}}}"#;
        assert!(parse_line(line).is_none());
    }

    #[test]
    fn test_parse_line_missing_timestamp() {
        let line = r#"{"type":"assistant","sessionId":"abc","cwd":"/tmp","message":{"model":"sonnet","role":"assistant","stop_reason":"end_turn","content":[],"usage":{"input_tokens":10,"output_tokens":20}}}"#;
        assert!(parse_line(line).is_none());
    }

    #[test]
    fn test_parse_line_zero_tokens() {
        let line = r#"{"type":"assistant","sessionId":"abc","timestamp":"2026-03-03T10:00:00Z","cwd":"/tmp","message":{"model":"sonnet","role":"assistant","stop_reason":"end_turn","content":[],"usage":{"input_tokens":0,"output_tokens":0,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}"#;
        let rec = parse_line(line).unwrap();
        assert_eq!(rec.input_tokens, 0);
        assert_eq!(rec.output_tokens, 0);
        assert_eq!(rec.cache_creation_tokens, 0);
        assert_eq!(rec.cache_read_tokens, 0);
    }

    #[test]
    fn test_parse_line_extracts_tool_names() {
        let line = r#"{"type":"assistant","sessionId":"abc","timestamp":"2026-03-03T10:00:00Z","cwd":"/tmp","message":{"model":"sonnet","role":"assistant","stop_reason":"end_turn","content":[{"type":"tool_use","name":"Bash","id":"t1","input":{}},{"type":"text","text":"hello"},{"type":"tool_use","name":"Read","id":"t2","input":{}}],"usage":{"input_tokens":10,"output_tokens":20}}}"#;
        let rec = parse_line(line).unwrap();
        assert_eq!(rec.tool_names, vec!["Bash", "Read"]);
        assert_eq!(rec.tool_use_ids, vec!["t1", "t2"]);
    }

    #[test]
    fn test_parse_line_no_tool_use_empty_vec() {
        let line = r#"{"type":"assistant","sessionId":"abc","timestamp":"2026-03-03T10:00:00Z","cwd":"/tmp","message":{"model":"sonnet","role":"assistant","stop_reason":"end_turn","content":[{"type":"text","text":"hello"}],"usage":{"input_tokens":10,"output_tokens":20}}}"#;
        let rec = parse_line(line).unwrap();
        assert!(rec.tool_names.is_empty());
        assert!(rec.tool_use_ids.is_empty());
    }

    #[test]
    fn test_parse_line_empty_content_array() {
        let line = r#"{"type":"assistant","sessionId":"abc","timestamp":"2026-03-03T10:00:00Z","cwd":"/tmp","message":{"model":"sonnet","role":"assistant","stop_reason":"end_turn","content":[],"usage":{"input_tokens":10,"output_tokens":20}}}"#;
        let rec = parse_line(line).unwrap();
        assert!(rec.tool_names.is_empty());
    }

    #[test]
    fn test_parse_line_extracts_git_branch() {
        let line = r#"{"type":"assistant","sessionId":"abc","timestamp":"2026-03-03T10:00:00Z","cwd":"/tmp","gitBranch":"feature/auth","message":{"model":"sonnet","role":"assistant","stop_reason":"end_turn","content":[],"usage":{"input_tokens":10,"output_tokens":20}}}"#;
        let rec = parse_line(line).unwrap();
        assert_eq!(rec.git_branch, "feature/auth");
    }

    #[test]
    fn test_parse_line_missing_git_branch_defaults_empty() {
        let line = r#"{"type":"assistant","sessionId":"abc","timestamp":"2026-03-03T10:00:00Z","cwd":"/tmp","message":{"model":"sonnet","role":"assistant","stop_reason":"end_turn","content":[],"usage":{"input_tokens":10,"output_tokens":20}}}"#;
        let rec = parse_line(line).unwrap();
        assert_eq!(rec.git_branch, "");
    }

    #[test]
    fn test_parse_line_skips_streaming_intermediate() {
        // Assistant line without stop_reason (streaming intermediate) — should be skipped
        let line = r#"{"type":"assistant","sessionId":"abc","timestamp":"2026-03-03T10:00:00Z","cwd":"/tmp","message":{"model":"sonnet","role":"assistant","content":[{"type":"text","text":"partial"}],"usage":{"input_tokens":100,"output_tokens":50}}}"#;
        assert!(parse_line(line).is_none());
    }

    #[test]
    fn test_parse_line_skips_null_stop_reason() {
        // Assistant line with stop_reason: null (streaming intermediate) — should be skipped
        let line = r#"{"type":"assistant","sessionId":"abc","timestamp":"2026-03-03T10:00:00Z","cwd":"/tmp","message":{"model":"sonnet","role":"assistant","stop_reason":null,"content":[{"type":"text","text":"partial"}],"usage":{"input_tokens":100,"output_tokens":50}}}"#;
        assert!(parse_line(line).is_none());
    }

    #[test]
    fn test_parse_line_accepts_tool_use_stop_reason() {
        // stop_reason: "tool_use" is a valid final line
        let line = r#"{"type":"assistant","sessionId":"abc","timestamp":"2026-03-03T10:00:00Z","cwd":"/tmp","message":{"model":"sonnet","role":"assistant","stop_reason":"tool_use","content":[{"type":"tool_use","name":"Bash","id":"t1","input":{}}],"usage":{"input_tokens":100,"output_tokens":50}}}"#;
        let rec = parse_line(line).unwrap();
        assert_eq!(rec.message_type, MessageType::Assistant);
        assert_eq!(rec.tool_names, vec!["Bash"]);
    }

    #[test]
    fn test_parse_line_accepts_max_tokens_stop_reason() {
        // stop_reason: "max_tokens" is a valid final line
        let line = r#"{"type":"assistant","sessionId":"abc","timestamp":"2026-03-03T10:00:00Z","cwd":"/tmp","message":{"model":"sonnet","role":"assistant","stop_reason":"max_tokens","content":[],"usage":{"input_tokens":100,"output_tokens":32000}}}"#;
        let rec = parse_line(line).unwrap();
        assert_eq!(rec.output_tokens, 32000);
    }

    // ── Assistant text length ──

    #[test]
    fn test_parse_assistant_text_length() {
        let line = r#"{"type":"assistant","sessionId":"abc","timestamp":"2026-03-03T10:00:00Z","cwd":"/tmp","message":{"model":"sonnet","role":"assistant","stop_reason":"end_turn","content":[{"type":"text","text":"Hello world!"}],"usage":{"input_tokens":10,"output_tokens":20}}}"#;
        let rec = parse_line(line).unwrap();
        assert_eq!(rec.text_length, 12); // "Hello world!" = 12 chars
    }

    #[test]
    fn test_parse_assistant_multiple_text_blocks() {
        let line = r#"{"type":"assistant","sessionId":"abc","timestamp":"2026-03-03T10:00:00Z","cwd":"/tmp","message":{"model":"sonnet","role":"assistant","stop_reason":"end_turn","content":[{"type":"text","text":"Hello"},{"type":"text","text":" world"}],"usage":{"input_tokens":10,"output_tokens":20}}}"#;
        let rec = parse_line(line).unwrap();
        assert_eq!(rec.text_length, 11); // "Hello" + " world" = 5 + 6
    }

    // ── User prompt tests ──

    #[test]
    fn test_parse_user_prompt_string_content() {
        let line = r#"{"type":"user","sessionId":"s1","timestamp":"2026-03-03T10:00:00Z","cwd":"/tmp","uuid":"u1","parentUuid":"p1","message":{"role":"user","content":"Fix the bug"}}"#;
        let rec = parse_line(line).unwrap();
        assert_eq!(rec.message_type, MessageType::UserPrompt);
        assert_eq!(rec.session_id, "s1");
        assert_eq!(rec.text_length, 11); // "Fix the bug" = 11 chars
        assert_eq!(rec.uuid, "u1");
        assert_eq!(rec.parent_uuid, "p1");
        assert_eq!(rec.input_tokens, 0);
        assert_eq!(rec.output_tokens, 0);
        assert!(rec.model.is_empty());
        assert!(rec.is_tool_error.is_none());
    }

    #[test]
    fn test_parse_user_prompt_list_content() {
        let line = r#"{"type":"user","sessionId":"s1","timestamp":"2026-03-03T10:00:00Z","cwd":"/tmp","uuid":"u2","parentUuid":"p2","message":{"role":"user","content":[{"type":"text","text":"Hello world"}]}}"#;
        let rec = parse_line(line).unwrap();
        assert_eq!(rec.message_type, MessageType::UserPrompt);
        assert_eq!(rec.text_length, 11); // "Hello world"
    }

    #[test]
    fn test_parse_user_prompt_empty_string() {
        let line = r#"{"type":"user","sessionId":"s1","timestamp":"2026-03-03T10:00:00Z","cwd":"/tmp","uuid":"u3","parentUuid":"p3","message":{"role":"user","content":""}}"#;
        let rec = parse_line(line).unwrap();
        assert_eq!(rec.message_type, MessageType::UserPrompt);
        assert_eq!(rec.text_length, 0);
    }

    #[test]
    fn test_parse_user_prompt_missing_session_id() {
        let line = r#"{"type":"user","timestamp":"2026-03-03T10:00:00Z","cwd":"/tmp","message":{"role":"user","content":"hi"}}"#;
        assert!(parse_line(line).is_none());
    }

    // ── Tool result tests ──

    #[test]
    fn test_parse_tool_result_success() {
        let line = r#"{"type":"user","sessionId":"s1","timestamp":"2026-03-03T10:05:00Z","cwd":"/tmp","uuid":"u4","parentUuid":"p4","toolUseResult":{"stdout":"output","stderr":"","interrupted":false},"message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_abc123","is_error":false,"content":"command output here"}]}}"#;
        let rec = parse_line(line).unwrap();
        assert_eq!(rec.message_type, MessageType::ToolResult);
        assert_eq!(rec.session_id, "s1");
        assert_eq!(rec.tool_use_ids, vec!["toolu_abc123"]);
        assert_eq!(rec.is_tool_error, Some(false));
        assert_eq!(rec.text_length, 19); // "command output here"
        assert_eq!(rec.input_tokens, 0);
    }

    #[test]
    fn test_parse_tool_result_error() {
        let line = r#"{"type":"user","sessionId":"s1","timestamp":"2026-03-03T10:05:00Z","cwd":"/tmp","uuid":"u5","parentUuid":"p5","toolUseResult":{"stdout":"","stderr":"err"},"message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_def456","is_error":true,"content":"Error: file not found"}]}}"#;
        let rec = parse_line(line).unwrap();
        assert_eq!(rec.message_type, MessageType::ToolResult);
        assert_eq!(rec.is_tool_error, Some(true));
        assert_eq!(rec.tool_use_ids, vec!["toolu_def456"]);
        assert_eq!(rec.text_length, 21); // "Error: file not found"
    }

    #[test]
    fn test_parse_tool_result_null_is_error() {
        // is_error can be null (None in JSON) — treat as not-error
        let line = r#"{"type":"user","sessionId":"s1","timestamp":"2026-03-03T10:05:00Z","cwd":"/tmp","uuid":"u6","parentUuid":"p6","toolUseResult":{"filenames":["a.rs"]},"message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_xyz","is_error":null,"content":"file contents"}]}}"#;
        let rec = parse_line(line).unwrap();
        assert_eq!(rec.is_tool_error, Some(false)); // null → not error
    }

    #[test]
    fn test_parse_tool_result_list_content() {
        // tool_result content can be a list of text blocks
        let line = r#"{"type":"user","sessionId":"s1","timestamp":"2026-03-03T10:05:00Z","cwd":"/tmp","uuid":"u7","parentUuid":"p7","toolUseResult":{"status":"completed"},"message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_agent","is_error":false,"content":[{"type":"text","text":"Result: "},{"type":"text","text":"success"}]}]}}"#;
        let rec = parse_line(line).unwrap();
        // "Result: " = 8 chars, "success" = 7 chars = 15
        assert_eq!(rec.text_length, 15);
    }

    #[test]
    fn test_parse_tool_result_missing_tool_use_result_field() {
        // user line without toolUseResult → parsed as user prompt, not tool result
        let line = r#"{"type":"user","sessionId":"s1","timestamp":"2026-03-03T10:00:00Z","cwd":"/tmp","uuid":"u8","parentUuid":"p8","message":{"role":"user","content":"hello"}}"#;
        let rec = parse_line(line).unwrap();
        assert_eq!(rec.message_type, MessageType::UserPrompt);
    }

    // ── parse_buffer with mixed types ──

    #[test]
    fn test_parse_buffer_mixed() {
        let buf = [
            "",                               // blank
            r#"{"type":"progress","data":{}}"#, // ignored type
            "{bad json}",                     // invalid
            r#"{"type":"user","sessionId":"a","timestamp":"2026-03-03T10:00:00Z","cwd":"/tmp","uuid":"u1","parentUuid":"p1","message":{"role":"user","content":"hello"}}"#, // user prompt
            r#"{"type":"assistant","sessionId":"a","timestamp":"2026-03-03T10:01:00Z","cwd":"/tmp","message":{"model":"sonnet","role":"assistant","stop_reason":"end_turn","content":[],"usage":{"input_tokens":10,"output_tokens":20}}}"#, // assistant
            "   ",                            // whitespace
            r#"{"type":"user","sessionId":"a","timestamp":"2026-03-03T10:02:00Z","cwd":"/tmp","uuid":"u2","parentUuid":"p2","toolUseResult":{"stdout":"ok"},"message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","is_error":false,"content":"done"}]}}"#, // tool result
        ].join("\n");

        let recs = parse_buffer(&buf);
        assert_eq!(recs.len(), 3);
        assert_eq!(recs[0].message_type, MessageType::UserPrompt);
        assert_eq!(recs[1].message_type, MessageType::Assistant);
        assert_eq!(recs[2].message_type, MessageType::ToolResult);
    }

    #[test]
    fn test_parse_buffer_only_assistant_unchanged() {
        // Existing behavior: buffer with only assistant lines still works
        let buf = [
            r#"{"type":"assistant","sessionId":"a","timestamp":"2026-03-03T10:00:00Z","cwd":"/tmp","message":{"model":"sonnet","role":"assistant","stop_reason":"end_turn","content":[],"usage":{"input_tokens":10,"output_tokens":20}}}"#,
            r#"{"type":"assistant","sessionId":"b","timestamp":"2026-03-03T11:00:00Z","cwd":"/tmp","message":{"model":"opus","role":"assistant","stop_reason":"end_turn","content":[],"usage":{"input_tokens":30,"output_tokens":40}}}"#,
        ].join("\n");

        let recs = parse_buffer(&buf);
        assert_eq!(recs.len(), 2);
        assert_eq!(recs[0].session_id, "a");
        assert_eq!(recs[1].session_id, "b");
    }
}
