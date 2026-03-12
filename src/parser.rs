use crate::heuristics;
use crate::types::{MessageRecord, MessageType, SubagentSpawn, ToolInputDetails, ToolOutputDetails};
use chrono::{DateTime, Utc};
use serde_json::Value;

/// Reasons a JSONL line may be skipped during parsing.
#[derive(Debug)]
pub enum ParseSkip {
    /// Empty or whitespace-only line.
    EmptyLine,
    /// Invalid JSON syntax.
    InvalidJson,
    /// Line type is not "assistant" or "user".
    UnknownType(String),
    /// Required field(s) missing or malformed.
    MissingField,
    /// Streaming intermediate line (no stop_reason).
    StreamingIntermediate,
}

/// Try to parse a single JSONL line into a MessageRecord.
/// Accepts `type: "assistant"` (with usage data), `type: "user"` (human prompt),
/// and `type: "user"` with `toolUseResult` (tool result).
/// Returns `Err(ParseSkip)` with the reason for skipped lines.
pub fn parse_line(line: &str) -> Result<MessageRecord, ParseSkip> {
    let line = line.trim();
    if line.is_empty() {
        return Err(ParseSkip::EmptyLine);
    }

    let v: Value = serde_json::from_str(line).map_err(|_| ParseSkip::InvalidJson)?;

    let line_type = v.get("type")
        .and_then(|t| t.as_str())
        .ok_or(ParseSkip::MissingField)?;

    match line_type {
        "assistant" => parse_assistant(&v),
        "user" => parse_user(&v),
        other => Err(ParseSkip::UnknownType(other.to_string())),
    }
}

/// Parse an assistant line (has usage data, model, tool_use blocks, text blocks).
fn parse_assistant(v: &Value) -> Result<MessageRecord, ParseSkip> {
    let session_id = v.get("sessionId")
        .and_then(|s| s.as_str())
        .ok_or(ParseSkip::MissingField)?
        .to_string();
    let cwd = v
        .get("cwd")
        .and_then(|c| c.as_str())
        .unwrap_or("unknown")
        .to_string();
    let timestamp: DateTime<Utc> = v.get("timestamp")
        .and_then(|t| t.as_str())
        .ok_or(ParseSkip::MissingField)?
        .parse()
        .map_err(|_| ParseSkip::MissingField)?;
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

    let message = v.get("message").ok_or(ParseSkip::MissingField)?;

    // Claude Code logs each streaming content block (thinking, text, tool_use) as a
    // separate assistant JSONL line. All lines from the same API turn carry identical
    // cumulative usage snapshots. Only the final line has stop_reason != null and
    // contains the correct accumulated totals. Skip intermediate streaming lines to
    // avoid counting usage 3-7x.
    let stop_reason = message.get("stop_reason");
    match stop_reason {
        Some(sr) if !sr.is_null() => {} // final line — process it
        _ => return Err(ParseSkip::StreamingIntermediate),
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

    let mut tool_input_details = ToolInputDetails::default();
    let mut has_tool_inputs = false;

    if let Some(arr) = content {
        for item in arr {
            match item.get("type").and_then(|t| t.as_str()) {
                Some("tool_use") => {
                    if let Some(name) = item.get("name").and_then(|n| n.as_str()) {
                        tool_names.push(name.to_string());
                        if let Some(input) = item.get("input") {
                            extract_tool_input(name, input, &mut tool_input_details);
                            has_tool_inputs = true;
                        }
                    }
                    if let Some(id) = item.get("id").and_then(|i| i.as_str()) {
                        tool_use_ids.push(id.to_string());
                    }
                }
                Some("text") => {
                    if let Some(txt) = item.get("text").and_then(|t| t.as_str()) {
                        text_length += txt.chars().count() as u64;
                        text_word_count += txt.split_whitespace().count() as u64;
                    }
                }
                _ => {}
            }
        }
    }

    let usage = message.get("usage").ok_or(ParseSkip::MissingField)?;

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

    Ok(MessageRecord {
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
        tool_input_details: if has_tool_inputs { Some(tool_input_details) } else { None },
        tool_output_details: None,
    })
}

/// Extract structured data from a tool_use input object.
fn extract_tool_input(tool_name: &str, input: &Value, details: &mut ToolInputDetails) {
    match tool_name {
        "Bash" => {
            if let Some(cmd) = input.get("command").and_then(|c| c.as_str()) {
                details.bash_commands.push(cmd.to_string());
            }
        }
        "Read" => {
            if let Some(fp) = input.get("file_path").and_then(|p| p.as_str()) {
                details.file_paths.push((fp.to_string(), "Read".to_string()));
            }
        }
        "Write" => {
            if let Some(fp) = input.get("file_path").and_then(|p| p.as_str()) {
                details.file_paths.push((fp.to_string(), "Write".to_string()));
            }
        }
        "Edit" => {
            if let Some(fp) = input.get("file_path").and_then(|p| p.as_str()) {
                details.file_paths.push((fp.to_string(), "Edit".to_string()));
            }
            let old_len = input
                .get("old_string")
                .and_then(|s| s.as_str())
                .map(|s| s.len() as u64)
                .unwrap_or(0);
            let new_len = input
                .get("new_string")
                .and_then(|s| s.as_str())
                .map(|s| s.len() as u64)
                .unwrap_or(0);
            if old_len > 0 || new_len > 0 {
                details.edit_sizes.push((old_len, new_len));
            }
        }
        "Grep" => {
            if let Some(pat) = input.get("pattern").and_then(|p| p.as_str()) {
                details.search_patterns.push(pat.to_string());
            }
            if let Some(fp) = input.get("path").and_then(|p| p.as_str()) {
                details.file_paths.push((fp.to_string(), "Grep".to_string()));
            }
        }
        "Glob" => {
            if let Some(pat) = input.get("pattern").and_then(|p| p.as_str()) {
                details.search_patterns.push(pat.to_string());
            }
            if let Some(fp) = input.get("path").and_then(|p| p.as_str()) {
                details.file_paths.push((fp.to_string(), "Glob".to_string()));
            }
        }
        "WebFetch" => {
            if let Some(url) = input.get("url").and_then(|u| u.as_str()) {
                details.urls.push(url.to_string());
            }
        }
        "WebSearch" => {
            if let Some(q) = input.get("query").and_then(|q| q.as_str()) {
                details.web_queries.push(q.to_string());
            }
        }
        "Agent" => {
            let subagent_type = input
                .get("subagent_type")
                .and_then(|s| s.as_str())
                .unwrap_or("unknown")
                .to_string();
            let description = input
                .get("description")
                .and_then(|d| d.as_str())
                .unwrap_or("")
                .to_string();
            let model = input
                .get("model")
                .and_then(|m| m.as_str())
                .map(String::from);
            details.subagent_spawns.push(SubagentSpawn {
                subagent_type,
                description,
                model,
            });
        }
        "TodoWrite" => {
            if let Ok(json) = serde_json::to_string(input) {
                details.todo_snapshots.push(json);
            }
        }
        _ => {}
    }
}

/// Parse a user line — either a human prompt or a tool result.
fn parse_user(v: &Value) -> Result<MessageRecord, ParseSkip> {
    let session_id = v.get("sessionId")
        .and_then(|s| s.as_str())
        .ok_or(ParseSkip::MissingField)?
        .to_string();
    let cwd = v
        .get("cwd")
        .and_then(|c| c.as_str())
        .unwrap_or("unknown")
        .to_string();
    let timestamp: DateTime<Utc> = v.get("timestamp")
        .and_then(|t| t.as_str())
        .ok_or(ParseSkip::MissingField)?
        .parse()
        .map_err(|_| ParseSkip::MissingField)?;
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
) -> Result<MessageRecord, ParseSkip> {
    let message = v.get("message").ok_or(ParseSkip::MissingField)?;
    let content = message.get("content").ok_or(ParseSkip::MissingField)?;

    let (text_length, text_word_count) = extract_text_stats(content);

    Ok(MessageRecord {
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
        tool_input_details: None,
        tool_output_details: None,
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
) -> Result<MessageRecord, ParseSkip> {
    let message = v.get("message").ok_or(ParseSkip::MissingField)?;
    let content = message.get("content").ok_or(ParseSkip::MissingField)?;

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

    // Extract tool output details from toolUseResult
    let tool_output_details = extract_tool_output(v.get("toolUseResult"));

    Ok(MessageRecord {
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
        tool_input_details: None,
        tool_output_details,
    })
}

/// Extract structured data from a toolUseResult output object.
fn extract_tool_output(tool_use_result: Option<&Value>) -> Option<ToolOutputDetails> {
    let result = tool_use_result?;
    let mut details = ToolOutputDetails::default();
    let mut has_data = false;

    if let Some(stdout) = result.get("stdout").and_then(|s| s.as_str()) {
        if !stdout.is_empty() {
            let truncated = if stdout.len() > heuristics::STDOUT_TRUNCATION_LIMIT {
                let mut end = heuristics::STDOUT_TRUNCATION_LIMIT;
                while end > 0 && !stdout.is_char_boundary(end) {
                    end -= 1;
                }
                &stdout[..end]
            } else {
                stdout
            };
            details.bash_stdout = Some(truncated.to_string());
            has_data = true;
        }
    }

    if let Some(rc) = result.get("returnCode").and_then(|r| r.as_i64()) {
        details.bash_return_code = Some(rc as i32);
        has_data = true;
    }

    if let Some(t) = result.get("type").and_then(|t| t.as_str()) {
        details.write_type = Some(t.to_string());
        has_data = true;
    }

    // Count patch additions/deletions from structuredPatch
    if let Some(patch) = result.get("structuredPatch") {
        if let Some(hunks) = patch.get("hunks").and_then(|h| h.as_array()) {
            for hunk in hunks {
                if let Some(lines) = hunk.get("lines").and_then(|l| l.as_array()) {
                    for line in lines {
                        if let Some(s) = line.as_str() {
                            if s.starts_with('+') {
                                details.patch_additions += 1;
                            } else if s.starts_with('-') {
                                details.patch_deletions += 1;
                            }
                        }
                    }
                }
            }
            has_data = true;
        }
    }

    if has_data {
        Some(details)
    } else {
        None
    }
}

/// Extract total text character length and word count from content.
/// Content can be a string or an array of `{type: "text", text: "..."}` blocks.
/// Returns (char_count, word_count).
fn extract_text_stats(content: &Value) -> (u64, u64) {
    if let Some(s) = content.as_str() {
        return (s.chars().count() as u64, s.split_whitespace().count() as u64);
    }
    if let Some(arr) = content.as_array() {
        let mut chars: u64 = 0;
        let mut words: u64 = 0;
        for item in arr {
            if item.get("type").and_then(|t| t.as_str()) == Some("text") {
                if let Some(txt) = item.get("text").and_then(|t| t.as_str()) {
                    chars += txt.chars().count() as u64;
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
    buf.lines().filter_map(|l| parse_line(l).ok()).collect()
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

/// Extract a home-relative project path from a cwd path.
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
        assert!(parse_line(line).is_err());
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
        assert!(parse_line("").is_err());
    }

    #[test]
    fn test_parse_line_whitespace_only() {
        assert!(parse_line("   \n").is_err());
    }

    #[test]
    fn test_parse_line_invalid_json() {
        assert!(parse_line("{not json}").is_err());
    }

    #[test]
    fn test_parse_line_missing_usage() {
        let line = r#"{"type":"assistant","sessionId":"abc","timestamp":"2026-03-03T10:00:00Z","cwd":"/tmp","message":{"model":"sonnet","role":"assistant","content":[]}}"#;
        assert!(parse_line(line).is_err());
    }

    #[test]
    fn test_parse_line_missing_session_id() {
        let line = r#"{"type":"assistant","timestamp":"2026-03-03T10:00:00Z","cwd":"/tmp","message":{"model":"sonnet","role":"assistant","stop_reason":"end_turn","content":[],"usage":{"input_tokens":10,"output_tokens":20}}}"#;
        assert!(parse_line(line).is_err());
    }

    #[test]
    fn test_parse_line_missing_timestamp() {
        let line = r#"{"type":"assistant","sessionId":"abc","cwd":"/tmp","message":{"model":"sonnet","role":"assistant","stop_reason":"end_turn","content":[],"usage":{"input_tokens":10,"output_tokens":20}}}"#;
        assert!(parse_line(line).is_err());
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
        assert!(parse_line(line).is_err());
    }

    #[test]
    fn test_parse_line_skips_null_stop_reason() {
        // Assistant line with stop_reason: null (streaming intermediate) — should be skipped
        let line = r#"{"type":"assistant","sessionId":"abc","timestamp":"2026-03-03T10:00:00Z","cwd":"/tmp","message":{"model":"sonnet","role":"assistant","stop_reason":null,"content":[{"type":"text","text":"partial"}],"usage":{"input_tokens":100,"output_tokens":50}}}"#;
        assert!(parse_line(line).is_err());
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
        assert!(parse_line(line).is_err());
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

    // ── Tool input extraction tests ──

    #[test]
    fn test_extract_bash_command() {
        let line = r#"{"type":"assistant","sessionId":"s1","timestamp":"2026-03-03T10:00:00Z","cwd":"/tmp","message":{"model":"sonnet","role":"assistant","stop_reason":"tool_use","content":[{"type":"tool_use","name":"Bash","id":"t1","input":{"command":"git status"}}],"usage":{"input_tokens":10,"output_tokens":20}}}"#;
        let rec = parse_line(line).unwrap();
        let details = rec.tool_input_details.unwrap();
        assert_eq!(details.bash_commands, vec!["git status"]);
    }

    #[test]
    fn test_extract_read_file_path() {
        let line = r#"{"type":"assistant","sessionId":"s1","timestamp":"2026-03-03T10:00:00Z","cwd":"/tmp","message":{"model":"sonnet","role":"assistant","stop_reason":"tool_use","content":[{"type":"tool_use","name":"Read","id":"t1","input":{"file_path":"/src/main.rs"}}],"usage":{"input_tokens":10,"output_tokens":20}}}"#;
        let rec = parse_line(line).unwrap();
        let details = rec.tool_input_details.unwrap();
        assert_eq!(details.file_paths, vec![("/src/main.rs".to_string(), "Read".to_string())]);
    }

    #[test]
    fn test_extract_edit_sizes() {
        let line = r#"{"type":"assistant","sessionId":"s1","timestamp":"2026-03-03T10:00:00Z","cwd":"/tmp","message":{"model":"sonnet","role":"assistant","stop_reason":"tool_use","content":[{"type":"tool_use","name":"Edit","id":"t1","input":{"file_path":"/a.rs","old_string":"hello","new_string":"hello world"}}],"usage":{"input_tokens":10,"output_tokens":20}}}"#;
        let rec = parse_line(line).unwrap();
        let details = rec.tool_input_details.unwrap();
        assert_eq!(details.edit_sizes, vec![(5, 11)]);
        assert_eq!(details.file_paths[0].1, "Edit");
    }

    #[test]
    fn test_extract_grep_pattern() {
        let line = r#"{"type":"assistant","sessionId":"s1","timestamp":"2026-03-03T10:00:00Z","cwd":"/tmp","message":{"model":"sonnet","role":"assistant","stop_reason":"tool_use","content":[{"type":"tool_use","name":"Grep","id":"t1","input":{"pattern":"fn main","path":"/src"}}],"usage":{"input_tokens":10,"output_tokens":20}}}"#;
        let rec = parse_line(line).unwrap();
        let details = rec.tool_input_details.unwrap();
        assert_eq!(details.search_patterns, vec!["fn main"]);
        assert_eq!(details.file_paths, vec![("/src".to_string(), "Grep".to_string())]);
    }

    #[test]
    fn test_extract_web_fetch_url() {
        let line = r#"{"type":"assistant","sessionId":"s1","timestamp":"2026-03-03T10:00:00Z","cwd":"/tmp","message":{"model":"sonnet","role":"assistant","stop_reason":"tool_use","content":[{"type":"tool_use","name":"WebFetch","id":"t1","input":{"url":"https://example.com","prompt":"read it"}}],"usage":{"input_tokens":10,"output_tokens":20}}}"#;
        let rec = parse_line(line).unwrap();
        let details = rec.tool_input_details.unwrap();
        assert_eq!(details.urls, vec!["https://example.com"]);
    }

    #[test]
    fn test_extract_web_search_query() {
        let line = r#"{"type":"assistant","sessionId":"s1","timestamp":"2026-03-03T10:00:00Z","cwd":"/tmp","message":{"model":"sonnet","role":"assistant","stop_reason":"tool_use","content":[{"type":"tool_use","name":"WebSearch","id":"t1","input":{"query":"rust async"}}],"usage":{"input_tokens":10,"output_tokens":20}}}"#;
        let rec = parse_line(line).unwrap();
        let details = rec.tool_input_details.unwrap();
        assert_eq!(details.web_queries, vec!["rust async"]);
    }

    #[test]
    fn test_extract_agent_spawn() {
        let line = r#"{"type":"assistant","sessionId":"s1","timestamp":"2026-03-03T10:00:00Z","cwd":"/tmp","message":{"model":"sonnet","role":"assistant","stop_reason":"tool_use","content":[{"type":"tool_use","name":"Agent","id":"t1","input":{"subagent_type":"Explore","description":"find files","model":"haiku"}}],"usage":{"input_tokens":10,"output_tokens":20}}}"#;
        let rec = parse_line(line).unwrap();
        let details = rec.tool_input_details.unwrap();
        assert_eq!(details.subagent_spawns.len(), 1);
        assert_eq!(details.subagent_spawns[0].subagent_type, "Explore");
        assert_eq!(details.subagent_spawns[0].model, Some("haiku".to_string()));
    }

    #[test]
    fn test_extract_multiple_tools() {
        let line = r#"{"type":"assistant","sessionId":"s1","timestamp":"2026-03-03T10:00:00Z","cwd":"/tmp","message":{"model":"sonnet","role":"assistant","stop_reason":"tool_use","content":[{"type":"tool_use","name":"Read","id":"t1","input":{"file_path":"/a.rs"}},{"type":"tool_use","name":"Write","id":"t2","input":{"file_path":"/b.rs","content":"hello"}}],"usage":{"input_tokens":10,"output_tokens":20}}}"#;
        let rec = parse_line(line).unwrap();
        let details = rec.tool_input_details.unwrap();
        assert_eq!(details.file_paths.len(), 2);
        assert_eq!(details.file_paths[0], ("/a.rs".to_string(), "Read".to_string()));
        assert_eq!(details.file_paths[1], ("/b.rs".to_string(), "Write".to_string()));
    }

    #[test]
    fn test_no_tool_input_when_no_tools() {
        let line = r#"{"type":"assistant","sessionId":"s1","timestamp":"2026-03-03T10:00:00Z","cwd":"/tmp","message":{"model":"sonnet","role":"assistant","stop_reason":"end_turn","content":[{"type":"text","text":"hello"}],"usage":{"input_tokens":10,"output_tokens":20}}}"#;
        let rec = parse_line(line).unwrap();
        assert!(rec.tool_input_details.is_none());
    }

    #[test]
    fn test_tool_output_bash_stdout() {
        let line = r#"{"type":"user","sessionId":"s1","timestamp":"2026-03-03T10:05:00Z","cwd":"/tmp","uuid":"u1","parentUuid":"p1","toolUseResult":{"stdout":"hello world","returnCode":0},"message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","is_error":false,"content":"hello world"}]}}"#;
        let rec = parse_line(line).unwrap();
        let details = rec.tool_output_details.unwrap();
        assert_eq!(details.bash_stdout, Some("hello world".to_string()));
        assert_eq!(details.bash_return_code, Some(0));
    }

    #[test]
    fn test_tool_output_truncates_stdout() {
        let long_output = "x".repeat(1000);
        let line = format!(
            r#"{{"type":"user","sessionId":"s1","timestamp":"2026-03-03T10:05:00Z","cwd":"/tmp","uuid":"u1","parentUuid":"p1","toolUseResult":{{"stdout":"{}","returnCode":0}},"message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"t1","is_error":false,"content":"ok"}}]}}}}"#,
            long_output
        );
        let rec = parse_line(&line).unwrap();
        let details = rec.tool_output_details.unwrap();
        assert_eq!(details.bash_stdout.as_ref().unwrap().len(), 500);
    }
}
