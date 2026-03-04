mod aggregator;
mod alerts;
mod config;
mod parser;
mod settings;
mod storage;
mod tray;
mod types;
mod ui;
mod watcher;

use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use eframe::egui;
use settings::Settings;
use types::MetricsState;

fn main() -> eframe::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let backfill = args.iter().any(|a| a == "--backfill");
    let tray_mode = args.iter().any(|a| a == "--tray");

    let settings = Settings::load();

    let projects_dir = match dirs::home_dir() {
        Some(home) => home.join(config::CLAUDE_PROJECTS_REL),
        None => {
            eprintln!("Could not determine home directory");
            std::process::exit(1);
        }
    };

    if !projects_dir.exists() {
        eprintln!(
            "Claude projects directory not found: {}",
            projects_dir.display()
        );
        eprintln!("Make sure Claude Code has been used at least once.");
        std::process::exit(1);
    }

    // Open SQLite database
    let db = match storage::Storage::open_default() {
        Ok(db) => db,
        Err(e) => {
            eprintln!("Failed to open database: {}", e);
            std::process::exit(1);
        }
    };
    let db = Arc::new(Mutex::new(db));

    // Backfill: rebuild SQLite from JSONL files on startup.
    // Uses rebuild_from_records() which clears existing data per-date before inserting,
    // preventing double-counting on repeated restarts.
    {
        println!("Backfilling database from JSONL files...");
        let mut bf_tracker = watcher::FileTracker::new();
        let scan_results = watcher::initial_scan(&projects_dir, &mut bf_tracker);
        let db_lock = db.lock().unwrap_or_else(|e| e.into_inner());
        let mut total_records = 0u64;
        // Collect all records for a single rebuild call per date group
        let all_records: Vec<&crate::types::MessageRecord> = scan_results
            .iter()
            .flat_map(|(_, records)| records.iter())
            .collect();
        if !all_records.is_empty() {
            // Group by date to rebuild each date atomically
            let mut by_date: std::collections::HashMap<String, Vec<&crate::types::MessageRecord>> =
                std::collections::HashMap::new();
            for rec in &all_records {
                let date = rec.timestamp.format("%Y-%m-%d").to_string();
                by_date.entry(date).or_default().push(rec);
            }
            for recs in by_date.values() {
                let owned: Vec<crate::types::MessageRecord> =
                    recs.iter().map(|r| (*r).clone()).collect();
                if let Err(e) = db_lock.rebuild_from_records(&owned) {
                    eprintln!("  Error rebuilding records: {}", e);
                }
            }
            total_records = all_records.len() as u64;
        }
        if let Ok(Some((min, max))) = db_lock.date_range() {
            println!(
                "Backfill complete: {} records, date range {} to {}",
                total_records, min, max
            );
        } else {
            println!("Backfill complete: {} records", total_records);
        }
        if backfill {
            std::process::exit(0);
        }
    }

    // Shared state between watcher thread and UI
    let state = Arc::new(Mutex::new(MetricsState::default()));

    // Always build today's live state from JSONL scan (authoritative source).
    // The DB backfill above handles historical data for sparklines.
    let mut tracker = watcher::FileTracker::new();
    {
        let scan_results = watcher::initial_scan(&projects_dir, &mut tracker);
        let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
        for (_path, records) in &scan_results {
            s.ingest(records, settings.idle_gap_minutes);
        }
        println!(
            "Initial scan: {} sessions, {} messages, ${:.2} estimated",
            s.sessions.len(),
            s.total_messages,
            s.estimated_cost(&settings)
        );
    }

    // Channel for new records from the file watcher
    let (tx, rx) = mpsc::channel();

    // Start filesystem watcher in background
    let _watcher = watcher::start_watcher(projects_dir, tracker, tx)
        .expect("Failed to start filesystem watcher");

    // Spawn a thread that drains the channel and updates shared state + DB
    let state_writer = Arc::clone(&state);
    let db_writer = Arc::clone(&db);
    let writer_idle_gap = settings.idle_gap_minutes;
    std::thread::spawn(move || {
        while let Ok(records) = rx.recv() {
            let mut s = state_writer.lock().unwrap_or_else(|e| e.into_inner());
            s.ingest(&records, writer_idle_gap);

            // Write-through to SQLite
            if let Ok(db_lock) = db_writer.lock() {
                let _ = db_lock.persist(&records);
            }
        }
    });

    // Launch egui window
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([settings.window_width, settings.window_height])
            .with_title("Claude Code Usage"),
        ..Default::default()
    };

    // Optionally spawn system tray
    let tray_state = if tray_mode {
        println!("Starting in system tray mode...");
        Some(tray::spawn_tray())
    } else {
        None
    };

    let projects_dir_for_ui = dirs::home_dir()
        .unwrap_or_default()
        .join(config::CLAUDE_PROJECTS_REL);

    eframe::run_native(
        "Claude Code Usage Card",
        options,
        Box::new(move |_cc| {
            Ok(Box::new(UsageApp {
                state,
                db,
                date_range_selection: DateRangeSelection::Today,
                prev_date_range: DateRangeSelection::Today,
                historical_data: None,
                alert_state: alerts::AlertState::new(),
                session_detail: ui::sessions::SessionDetailState::new(projects_dir_for_ui),
                tray_state,
                settings,
                settings_modal: None,
                cached_state: MetricsState::default(),
            }))
        }),
    )
}

/// Date range presets for the selector.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DateRangeSelection {
    Today,
    Last7,
    Last30,
    AllTime,
}

impl DateRangeSelection {
    pub fn label(&self) -> &str {
        match self {
            Self::Today => "Today",
            Self::Last7 => "Last 7 Days",
            Self::Last30 => "Last 30 Days",
            Self::AllTime => "All Time",
        }
    }

    /// (start_date_str, end_date_str)
    pub fn date_range(&self) -> (String, String) {
        let end = storage::today_str();
        let start = match self {
            Self::Today => end.clone(),
            Self::Last7 => storage::days_ago(7),
            Self::Last30 => storage::days_ago(30),
            Self::AllTime => "2020-01-01".to_string(),
        };
        (start, end)
    }
}

/// Cached historical data for sparklines.
#[derive(Debug, Clone)]
pub struct HistoricalData {
    /// (date, total_tokens, messages) per day
    pub daily_totals: Vec<(String, u64, f64)>,
    /// Per-project: project_name -> Vec<(date, total_tokens)>
    pub project_trends: std::collections::HashMap<String, Vec<(String, u64)>>,
}

struct UsageApp {
    state: Arc<Mutex<MetricsState>>,
    db: Arc<Mutex<storage::Storage>>,
    date_range_selection: DateRangeSelection,
    prev_date_range: DateRangeSelection,
    historical_data: Option<HistoricalData>,
    alert_state: alerts::AlertState,
    session_detail: ui::sessions::SessionDetailState,
    tray_state: Option<tray::TrayState>,
    settings: Settings,
    settings_modal: Option<ui::settings_modal::SettingsModal>,
    /// Cached clone of MetricsState, only refreshed when dirty flag is set.
    cached_state: MetricsState,
}

impl UsageApp {
    fn refresh_historical(&mut self) {
        if self.date_range_selection == DateRangeSelection::Today {
            self.historical_data = None;
            return;
        }

        let (start, end) = self.date_range_selection.date_range();
        let db_lock = match self.db.lock() {
            Ok(db) => db,
            Err(_) => return,
        };

        let daily_totals = db_lock.daily_totals(&start, &end).unwrap_or_default();

        // Get project names from current state
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let project_names: Vec<String> = state.projects.keys().cloned().collect();
        drop(state);

        let mut project_trends = std::collections::HashMap::new();
        for proj in &project_names {
            if let Ok(trend) = db_lock.project_daily_totals(proj, &start, &end) {
                if !trend.is_empty() {
                    project_trends.insert(proj.clone(), trend);
                }
            }
        }

        self.historical_data = Some(HistoricalData {
            daily_totals,
            project_trends,
        });
    }
}

impl eframe::App for UsageApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Request repaint every 2 seconds to reflect watcher updates
        ctx.request_repaint_after(std::time::Duration::from_secs(2));

        // Handle tray signals
        if let Some(ref tray) = self.tray_state {
            use std::sync::atomic::Ordering;

            if tray.want_quit.load(Ordering::Relaxed) {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                return;
            }

            let visible = tray.want_visible.load(Ordering::Relaxed);
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(!visible));
            if visible {
                ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            }
        }

        // Prune burn window and refresh cached state only when dirty
        {
            let mut s = self
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            s.prune_burn_window(self.settings.burn_rate_window_minutes);
            if s.dirty {
                self.cached_state = s.clone();
                s.dirty = false;
            }
        }

        let state = &self.cached_state;

        // Check cost alert thresholds
        let cost = state.estimated_cost(&self.settings);
        self.alert_state.check(cost, &self.settings);

        // Update tray title with summary
        if let Some(ref tray) = self.tray_state {
            let sessions = state.sessions.len();
            let _ = tray
                .title_tx
                .send(format!("${:.2} | {} sess", cost, sessions));
        }

        let gear_clicked = ui::render(
            ctx,
            state,
            &self.settings,
            &mut self.date_range_selection,
            &self.historical_data,
            &mut self.session_detail,
        );

        // Open settings modal on gear click
        if gear_clicked && self.settings_modal.is_none() {
            self.settings_modal = Some(ui::settings_modal::SettingsModal::new(&self.settings));
        }

        // Render settings modal if open
        if let Some(ref mut modal) = self.settings_modal {
            if !modal.render(ctx, &mut self.settings) {
                self.settings_modal = None;
            }
        }

        // Detect date range change and refresh historical data
        if self.date_range_selection != self.prev_date_range {
            self.prev_date_range = self.date_range_selection;
            self.refresh_historical();
        }
    }
}
