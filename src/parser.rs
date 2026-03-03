use crate::types::MessageRecord;
use chrono::{DateTime, Utc};
use serde_json::Value;

/// Try to parse a single JSONL line into a MessageRecord.
/// Returns None for non-assistant messages, messages without usage, or malformed lines.
pub fn parse_line(line: &str) -> Option<MessageRecord> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }

    let v: Value = serde_json::from_str(line).ok()?;

    // Only care about assistant messages (they have usage data)
    if v.get("type")?.as_str()? != "assistant" {
        return None;
    }

    let session_id = v.get("sessionId")?.as_str()?.to_string();
    let cwd = v.get("cwd").and_then(|c| c.as_str()).unwrap_or("unknown").to_string();
    let timestamp_str = v.get("timestamp")?.as_str()?;
    let timestamp: DateTime<Utc> = timestamp_str.parse().ok()?;

    let message = v.get("message")?;
    let model = message
        .get("model")
        .and_then(|m| m.as_str())
        .unwrap_or("unknown")
        .to_string();

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
    })
}

/// Parse all lines from a buffer, returning only valid MessageRecords.
pub fn parse_buffer(buf: &str) -> Vec<MessageRecord> {
    buf.lines().filter_map(parse_line).collect()
}

/// Decode a Claude Code project directory name to a human-readable path.
/// e.g. "-home-bbeierle12-Agent-Shell" -> "/home/bbeierle12/Agent-Shell"
pub fn decode_project_dir(dir_name: &str) -> String {
    if dir_name.starts_with('-') {
        dir_name.replacen('-', "/", 1).replace('-', "/")
    } else {
        dir_name.replace('-', "/")
    }
}

/// Extract a short project name from a cwd path.
/// "/home/bbeierle12/Agent-Shell" -> "Agent-Shell"
/// "/home/bbeierle12" -> "~"
pub fn short_project_name(cwd: &str) -> String {
    let home = dirs::home_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();

    if cwd == home {
        return "~".to_string();
    }

    cwd.strip_prefix(&home)
        .and_then(|rest| rest.strip_prefix('/'))
        .unwrap_or(cwd)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_decode_project_dir() {
        assert_eq!(
            decode_project_dir("-home-bbeierle12-Agent-Shell"),
            "/home/bbeierle12/Agent/Shell"
        );
    }

    #[test]
    fn test_short_project_name() {
        let home = dirs::home_dir().unwrap().to_string_lossy().to_string();
        assert_eq!(short_project_name(&home), "~");
        assert_eq!(
            short_project_name(&format!("{}/Agent-Shell", home)),
            "Agent-Shell"
        );
    }

    #[test]
    fn test_parse_line_non_assistant() {
        let line = r#"{"type":"user","sessionId":"abc","timestamp":"2026-03-03T00:00:00Z","cwd":"/tmp","message":{"role":"user","content":"hi"}}"#;
        assert!(parse_line(line).is_none());
    }

    #[test]
    fn test_parse_line_assistant_with_usage() {
        let line = r#"{"type":"assistant","sessionId":"abc-123","timestamp":"2026-03-03T10:30:00Z","cwd":"/home/user/proj","message":{"model":"claude-opus-4-6","role":"assistant","content":[],"usage":{"input_tokens":100,"output_tokens":200,"cache_creation_input_tokens":50,"cache_read_input_tokens":1000}}}"#;
        let rec = parse_line(line).unwrap();
        assert_eq!(rec.session_id, "abc-123");
        assert_eq!(rec.model, "claude-opus-4-6");
        assert_eq!(rec.input_tokens, 100);
        assert_eq!(rec.output_tokens, 200);
        assert_eq!(rec.cache_creation_tokens, 50);
        assert_eq!(rec.cache_read_tokens, 1000);
    }
}
