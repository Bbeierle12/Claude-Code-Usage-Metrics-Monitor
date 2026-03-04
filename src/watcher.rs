use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::mpsc;

use crate::parser;
use crate::types::MessageRecord;

/// Tracks byte offsets for incremental reads.
pub struct FileTracker {
    offsets: HashMap<PathBuf, u64>,
}

impl FileTracker {
    pub fn new() -> Self {
        Self {
            offsets: HashMap::new(),
        }
    }

    /// Read new lines from a file starting at the last known offset.
    /// Returns parsed MessageRecords and updates the offset.
    pub fn read_new_lines(&mut self, path: &Path) -> Vec<MessageRecord> {
        let mut records = Vec::new();

        let file = match File::open(path) {
            Ok(f) => f,
            Err(_) => return records,
        };

        let metadata = match file.metadata() {
            Ok(m) => m,
            Err(_) => return records,
        };

        let file_len = metadata.len();
        let offset = self.offsets.get(path).copied().unwrap_or(0);

        // File was truncated or replaced — re-read from beginning
        let seek_pos = if offset > file_len { 0 } else { offset };

        let mut reader = BufReader::new(file);
        if reader.seek(SeekFrom::Start(seek_pos)).is_err() {
            return records;
        }

        let mut line = String::new();
        let mut current_pos = seek_pos;

        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) => break, // EOF
                Ok(n) => {
                    current_pos += n as u64;
                    if let Some(rec) = parser::parse_line(&line) {
                        records.push(rec);
                    }
                }
                Err(_) => break,
            }
        }

        self.offsets.insert(path.to_path_buf(), current_pos);
        records
    }

    /// Record the current file length as the offset (marks file as "already read").
    #[cfg(test)]
    pub fn mark_fully_read(&mut self, path: &Path) {
        if let Ok(meta) = std::fs::metadata(path) {
            self.offsets.insert(path.to_path_buf(), meta.len());
        }
    }
}

/// Scan all existing .jsonl files under the projects directory.
/// Also records offsets in the tracker so subsequent watcher events
/// don't re-read already-ingested content.
pub fn initial_scan(
    projects_dir: &Path,
    tracker: &mut FileTracker,
) -> Vec<(PathBuf, Vec<MessageRecord>)> {
    let mut results = Vec::new();
    scan_dir_recursive(projects_dir, tracker, &mut results);
    results
}

fn scan_dir_recursive(
    dir: &Path,
    tracker: &mut FileTracker,
    results: &mut Vec<(PathBuf, Vec<MessageRecord>)>,
) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            scan_dir_recursive(&path, tracker, results);
        } else if path.extension().is_some_and(|e| e == "jsonl") {
            // Use read_new_lines for streaming line-by-line parsing
            // (avoids loading entire file into memory for large JSONL files)
            let records = tracker.read_new_lines(&path);
            if !records.is_empty() {
                results.push((path, records));
            }
        }
    }
}

/// Start a filesystem watcher that sends new MessageRecords through the channel.
/// Takes ownership of the FileTracker (with offsets from initial_scan) to avoid
/// duplicate reads.
/// Returns the watcher (must be kept alive).
pub fn start_watcher(
    projects_dir: PathBuf,
    tracker: FileTracker,
    tx: mpsc::Sender<Vec<MessageRecord>>,
) -> notify::Result<RecommendedWatcher> {
    // Move tracker into the closure via a Mutex so it can be mutated in the callback
    let tracker = std::sync::Mutex::new(tracker);

    let mut watcher = notify::recommended_watcher(move |res: Result<Event, notify::Error>| {
        let event = match res {
            Ok(e) => e,
            Err(_) => return,
        };

        match event.kind {
            EventKind::Create(_) | EventKind::Modify(_) => {}
            _ => return,
        }

        let mut tracker = match tracker.lock() {
            Ok(t) => t,
            Err(_) => return,
        };

        for path in &event.paths {
            if path.extension().is_some_and(|e| e == "jsonl") {
                let records = tracker.read_new_lines(path);
                if !records.is_empty() {
                    let _ = tx.send(records);
                }
            }
        }
    })?;

    watcher.watch(&projects_dir, RecursiveMode::Recursive)?;

    Ok(watcher)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use std::fs;
    use std::io::Write;

    /// Produce a valid JSONL assistant line with usage data (final, with stop_reason).
    fn make_jsonl_line(session_id: &str, model: &str, input: u64, output: u64) -> String {
        let ts = Utc::now().to_rfc3339();
        format!(
            r#"{{"type":"assistant","sessionId":"{}","timestamp":"{}","cwd":"/tmp/proj","message":{{"model":"{}","role":"assistant","stop_reason":"end_turn","content":[],"usage":{{"input_tokens":{},"output_tokens":{},"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}}}}"#,
            session_id, ts, model, input, output
        )
    }

    /// Produce a JSONL assistant line with tool_use content blocks and gitBranch.
    fn make_jsonl_line_full(
        session_id: &str,
        model: &str,
        input: u64,
        output: u64,
        tools: &[&str],
        branch: &str,
    ) -> String {
        let ts = Utc::now().to_rfc3339();
        let content_items: Vec<String> = tools
            .iter()
            .enumerate()
            .map(|(i, name)| {
                format!(
                    r#"{{"type":"tool_use","id":"t{}","name":"{}","input":{{}}}}"#,
                    i, name
                )
            })
            .collect();
        let content = format!("[{}]", content_items.join(","));
        let branch_field = if branch.is_empty() {
            String::new()
        } else {
            format!(r#","gitBranch":"{}""#, branch)
        };
        format!(
            r#"{{"type":"assistant","sessionId":"{}","timestamp":"{}","cwd":"/tmp/proj"{},"message":{{"model":"{}","role":"assistant","stop_reason":"end_turn","content":{},"usage":{{"input_tokens":{},"output_tokens":{},"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}}}}"#,
            session_id, ts, branch_field, model, content, input, output
        )
    }

    fn make_user_line(session_id: &str) -> String {
        let ts = Utc::now().to_rfc3339();
        format!(
            r#"{{"type":"user","sessionId":"{}","timestamp":"{}","cwd":"/tmp/proj","message":{{"role":"user","content":"hello"}}}}"#,
            session_id, ts
        )
    }

    fn temp_jsonl(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cuc_test_{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    // ── FileTracker unit tests ────────────────────────────

    #[test]
    fn test_read_new_lines_fresh_file() {
        let path = temp_jsonl("fresh.jsonl");
        let mut f = fs::File::create(&path).unwrap();
        writeln!(f, "{}", make_jsonl_line("s1", "sonnet", 10, 20)).unwrap();
        writeln!(f, "{}", make_jsonl_line("s1", "sonnet", 30, 40)).unwrap();

        let mut tracker = FileTracker::new();
        let recs = tracker.read_new_lines(&path);
        assert_eq!(recs.len(), 2);
        assert_eq!(recs[0].input_tokens, 10);
        assert_eq!(recs[1].input_tokens, 30);

        fs::remove_file(&path).ok();
    }

    #[test]
    fn test_read_new_lines_incremental() {
        let path = temp_jsonl("incr.jsonl");
        let mut f = fs::File::create(&path).unwrap();
        writeln!(f, "{}", make_jsonl_line("s1", "sonnet", 10, 20)).unwrap();
        drop(f);

        let mut tracker = FileTracker::new();
        let recs = tracker.read_new_lines(&path);
        assert_eq!(recs.len(), 1);

        // Append a second line
        let mut f = fs::OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(f, "{}", make_jsonl_line("s1", "sonnet", 30, 40)).unwrap();
        drop(f);

        let recs = tracker.read_new_lines(&path);
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].input_tokens, 30);

        fs::remove_file(&path).ok();
    }

    #[test]
    fn test_read_new_lines_no_double_read() {
        let path = temp_jsonl("nodup.jsonl");
        let mut f = fs::File::create(&path).unwrap();
        writeln!(f, "{}", make_jsonl_line("s1", "sonnet", 10, 20)).unwrap();
        writeln!(f, "{}", make_jsonl_line("s1", "sonnet", 30, 40)).unwrap();
        drop(f);

        let mut tracker = FileTracker::new();
        let recs = tracker.read_new_lines(&path);
        assert_eq!(recs.len(), 2);

        // Second read without append → 0
        let recs = tracker.read_new_lines(&path);
        assert_eq!(recs.len(), 0);

        fs::remove_file(&path).ok();
    }

    #[test]
    fn test_read_new_lines_truncated_file() {
        let path = temp_jsonl("trunc.jsonl");
        let mut f = fs::File::create(&path).unwrap();
        writeln!(f, "{}", make_jsonl_line("s1", "sonnet", 10, 20)).unwrap();
        writeln!(f, "{}", make_jsonl_line("s1", "sonnet", 30, 40)).unwrap();
        writeln!(f, "{}", make_jsonl_line("s1", "sonnet", 50, 60)).unwrap();
        drop(f);

        let mut tracker = FileTracker::new();
        let _ = tracker.read_new_lines(&path);

        // Truncate and write 1 new line
        let mut f = fs::File::create(&path).unwrap();
        writeln!(f, "{}", make_jsonl_line("s2", "opus", 99, 88)).unwrap();
        drop(f);

        let recs = tracker.read_new_lines(&path);
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].input_tokens, 99);

        fs::remove_file(&path).ok();
    }

    #[test]
    fn test_read_new_lines_nonexistent_file() {
        let path = std::env::temp_dir().join("cuc_does_not_exist.jsonl");
        let mut tracker = FileTracker::new();
        let recs = tracker.read_new_lines(&path);
        assert!(recs.is_empty());
    }

    #[test]
    fn test_mark_fully_read() {
        let path = temp_jsonl("markread.jsonl");
        let mut f = fs::File::create(&path).unwrap();
        writeln!(f, "{}", make_jsonl_line("s1", "sonnet", 10, 20)).unwrap();
        writeln!(f, "{}", make_jsonl_line("s1", "sonnet", 30, 40)).unwrap();
        drop(f);

        let mut tracker = FileTracker::new();
        tracker.mark_fully_read(&path);

        let recs = tracker.read_new_lines(&path);
        assert_eq!(recs.len(), 0);

        fs::remove_file(&path).ok();
    }

    #[test]
    fn test_read_new_lines_parses_mixed_types() {
        let path = temp_jsonl("mixed_roles.jsonl");
        let mut f = fs::File::create(&path).unwrap();
        writeln!(f, "{}", make_user_line("s1")).unwrap();
        writeln!(f, "{}", make_jsonl_line("s1", "sonnet", 10, 20)).unwrap();
        writeln!(f, "{}", make_user_line("s1")).unwrap();
        writeln!(f, "{}", make_jsonl_line("s1", "sonnet", 30, 40)).unwrap();
        drop(f);

        let mut tracker = FileTracker::new();
        let recs = tracker.read_new_lines(&path);
        assert_eq!(recs.len(), 4); // user + assistant lines all parse
        assert_eq!(recs[0].message_type, crate::types::MessageType::UserPrompt);
        assert_eq!(recs[1].message_type, crate::types::MessageType::Assistant);
        assert_eq!(recs[1].input_tokens, 10);
        assert_eq!(recs[3].input_tokens, 30);

        fs::remove_file(&path).ok();
    }

    // ── initial_scan integration tests ────────────────────

    #[test]
    fn test_initial_scan_finds_nested_jsonl() {
        let root = std::env::temp_dir().join(format!("cuc_scan_{}_nested", std::process::id()));
        let dir_a = root.join("proj-a");
        let dir_b = root.join("proj-b");
        fs::create_dir_all(&dir_a).unwrap();
        fs::create_dir_all(&dir_b).unwrap();

        let mut fa = fs::File::create(dir_a.join("s.jsonl")).unwrap();
        writeln!(fa, "{}", make_jsonl_line("sa", "sonnet", 10, 20)).unwrap();
        drop(fa);

        let mut fb = fs::File::create(dir_b.join("s.jsonl")).unwrap();
        writeln!(fb, "{}", make_jsonl_line("sb", "opus", 30, 40)).unwrap();
        drop(fb);

        let mut tracker = FileTracker::new();
        let results = initial_scan(&root, &mut tracker);

        assert_eq!(results.len(), 2);
        let all_sessions: Vec<&str> = results
            .iter()
            .flat_map(|(_, recs)| recs.iter().map(|r| r.session_id.as_str()))
            .collect();
        assert!(all_sessions.contains(&"sa"));
        assert!(all_sessions.contains(&"sb"));

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn test_initial_scan_ignores_non_jsonl() {
        let root = std::env::temp_dir().join(format!("cuc_scan_{}_ext", std::process::id()));
        fs::create_dir_all(&root).unwrap();

        // .txt file should be ignored
        fs::write(root.join("notes.txt"), "not a jsonl file").unwrap();

        let mut f = fs::File::create(root.join("s.jsonl")).unwrap();
        writeln!(f, "{}", make_jsonl_line("s1", "sonnet", 10, 20)).unwrap();
        drop(f);

        let mut tracker = FileTracker::new();
        let results = initial_scan(&root, &mut tracker);

        assert_eq!(results.len(), 1);
        assert!(results[0].0.extension().unwrap() == "jsonl");

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn test_initial_scan_records_offsets() {
        let root = std::env::temp_dir().join(format!("cuc_scan_{}_off", std::process::id()));
        fs::create_dir_all(&root).unwrap();

        let path = root.join("s.jsonl");
        let mut f = fs::File::create(&path).unwrap();
        writeln!(f, "{}", make_jsonl_line("s1", "sonnet", 10, 20)).unwrap();
        drop(f);

        let mut tracker = FileTracker::new();
        let _ = initial_scan(&root, &mut tracker);

        // After scan, offsets are recorded — read_new_lines returns 0
        let recs = tracker.read_new_lines(&path);
        assert_eq!(recs.len(), 0);

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn test_initial_scan_empty_dir() {
        let root = std::env::temp_dir().join(format!("cuc_scan_{}_empty", std::process::id()));
        fs::create_dir_all(&root).unwrap();

        let mut tracker = FileTracker::new();
        let results = initial_scan(&root, &mut tracker);

        assert!(results.is_empty());

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn test_read_new_lines_with_tools_and_branch() {
        let path = temp_jsonl("tools_branch.jsonl");
        let mut f = fs::File::create(&path).unwrap();
        writeln!(
            f,
            "{}",
            make_jsonl_line_full("s1", "sonnet", 10, 20, &["Bash", "Read"], "feature/auth")
        )
        .unwrap();
        drop(f);

        let mut tracker = FileTracker::new();
        let recs = tracker.read_new_lines(&path);
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].tool_names, vec!["Bash", "Read"]);
        assert_eq!(recs[0].git_branch, "feature/auth");

        fs::remove_file(&path).ok();
    }
}
