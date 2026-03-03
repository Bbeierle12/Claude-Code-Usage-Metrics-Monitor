mod aggregator;
mod config;
mod parser;
mod types;
mod ui;
mod watcher;

use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use eframe::egui;
use types::MetricsState;

fn main() -> eframe::Result<()> {
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

    // Shared state between watcher thread and UI
    let state = Arc::new(Mutex::new(MetricsState::default()));

    // Initial scan — parse all existing JSONL files
    {
        let scan_results = watcher::initial_scan(&projects_dir);
        let mut s = state.lock().unwrap();
        for (_path, records) in &scan_results {
            s.ingest(records);
        }
        println!(
            "Initial scan: {} sessions, {} messages, ${:.2} estimated",
            s.sessions.len(),
            s.total_messages,
            s.estimated_cost()
        );
    }

    // Channel for new records from the file watcher
    let (tx, rx) = mpsc::channel();

    // Start filesystem watcher in background
    let _watcher = watcher::start_watcher(projects_dir, tx)
        .expect("Failed to start filesystem watcher");

    // Spawn a thread that drains the channel and updates shared state
    let state_writer = Arc::clone(&state);
    std::thread::spawn(move || {
        while let Ok(records) = rx.recv() {
            let mut s = state_writer.lock().unwrap();
            s.ingest(&records);
        }
    });

    // Launch egui window
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([config::WINDOW_WIDTH, config::WINDOW_HEIGHT])
            .with_title("Claude Code Usage"),
        ..Default::default()
    };

    eframe::run_native(
        "Claude Code Usage Card",
        options,
        Box::new(move |_cc| Ok(Box::new(UsageApp { state }))),
    )
}

struct UsageApp {
    state: Arc<Mutex<MetricsState>>,
}

impl eframe::App for UsageApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Request repaint every 2 seconds to reflect watcher updates
        ctx.request_repaint_after(std::time::Duration::from_secs(2));

        let state = self.state.lock().unwrap().clone();
        ui::render(ctx, &state);
    }
}
