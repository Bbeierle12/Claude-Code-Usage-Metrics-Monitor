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
}

/// Scan all existing .jsonl files under the projects directory.
pub fn initial_scan(projects_dir: &Path) -> Vec<(PathBuf, Vec<MessageRecord>)> {
    let mut results = Vec::new();
    scan_dir_recursive(projects_dir, &mut results);
    results
}

fn scan_dir_recursive(dir: &Path, results: &mut Vec<(PathBuf, Vec<MessageRecord>)>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            scan_dir_recursive(&path, results);
        } else if path.extension().map_or(false, |e| e == "jsonl") {
            let content = match std::fs::read_to_string(&path) {
                Ok(c) => c,
                Err(_) => continue,
            };
            let records = parser::parse_buffer(&content);
            if !records.is_empty() {
                results.push((path, records));
            }
        }
    }
}

/// Start a filesystem watcher that sends new MessageRecords through the channel.
/// Returns the watcher (must be kept alive) and a handle.
pub fn start_watcher(
    projects_dir: PathBuf,
    tx: mpsc::Sender<Vec<MessageRecord>>,
) -> notify::Result<RecommendedWatcher> {
    let mut tracker = FileTracker::new();

    let mut watcher = notify::recommended_watcher(move |res: Result<Event, notify::Error>| {
        let event = match res {
            Ok(e) => e,
            Err(_) => return,
        };

        match event.kind {
            EventKind::Create(_) | EventKind::Modify(_) => {}
            _ => return,
        }

        for path in &event.paths {
            if path.extension().map_or(false, |e| e == "jsonl") {
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
