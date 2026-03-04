use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;

use tray_icon::menu::{Menu, MenuEvent, MenuItem};
use tray_icon::{Icon, TrayIconBuilder};

/// Shared state between the tray thread and the egui app.
pub struct TrayState {
    pub want_visible: Arc<AtomicBool>,
    pub want_quit: Arc<AtomicBool>,
    /// Channel to update the tray title/tooltip dynamically.
    pub title_tx: mpsc::Sender<String>,
}

/// Create a simple 16x16 colored icon in memory (no PNG file needed).
fn make_icon() -> Icon {
    let size = 16u32;
    let mut rgba = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            // Simple circle: purple (#B464FF) inside, transparent outside
            let cx = (x as f32) - 7.5;
            let cy = (y as f32) - 7.5;
            let dist = (cx * cx + cy * cy).sqrt();
            if dist < 7.0 {
                rgba.extend_from_slice(&[180, 100, 255, 255]); // purple
            } else {
                rgba.extend_from_slice(&[0, 0, 0, 0]); // transparent
            }
        }
    }
    Icon::from_rgba(rgba, size, size).expect("Failed to create tray icon")
}

/// Spawn the tray icon. Returns the shared state for the egui app.
/// The tray icon lives on a background thread (Linux requires GTK event loop).
pub fn spawn_tray() -> TrayState {
    let want_visible = Arc::new(AtomicBool::new(false)); // start minimized in tray mode
    let want_quit = Arc::new(AtomicBool::new(false));
    let (title_tx, title_rx) = mpsc::channel::<String>();

    let vis = want_visible.clone();
    let quit = want_quit.clone();

    std::thread::spawn(move || {
        let icon = make_icon();
        let menu = Menu::new();
        let show_item = MenuItem::new("Show / Hide", true, None);
        let quit_item = MenuItem::new("Quit", true, None);
        menu.append(&show_item).ok();
        menu.append(&quit_item).ok();

        let show_id = show_item.id().clone();
        let quit_id = quit_item.id().clone();

        let tray = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_title("Claude Usage")
            .with_tooltip("Claude Code Usage Card")
            .with_icon(icon)
            .build()
            .expect("Failed to build tray icon");

        // Event loop: block on menu events with timeout, check title updates
        let menu_rx = MenuEvent::receiver();
        loop {
            // Block with timeout to allow checking title updates
            if let Ok(event) = menu_rx.recv_timeout(std::time::Duration::from_secs(2)) {
                if event.id == show_id {
                    let current = vis.load(Ordering::Relaxed);
                    vis.store(!current, Ordering::Relaxed);
                } else if event.id == quit_id {
                    quit.store(true, Ordering::Relaxed);
                    break;
                }
            }

            // Drain title updates (take the latest)
            let mut latest_title = None;
            while let Ok(new_title) = title_rx.try_recv() {
                latest_title = Some(new_title);
            }
            if let Some(title) = latest_title {
                tray.set_title(Some(&title));
            }
        }

        drop(tray);
    });

    TrayState {
        want_visible,
        want_quit,
        title_tx,
    }
}
