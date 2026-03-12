use std::collections::HashSet;

use crate::settings::Settings;

/// Alert threshold levels.
#[derive(Debug, Clone, Copy, Hash, Eq, PartialEq)]
pub enum ThresholdLevel {
    Warn,
    Critical,
}

/// Tracks which thresholds have been crossed today. Resets on date change.
pub struct AlertState {
    fired: HashSet<ThresholdLevel>,
    current_date: String,
}

impl AlertState {
    pub fn new() -> Self {
        Self {
            fired: HashSet::new(),
            current_date: chrono::Utc::now().format("%Y-%m-%d").to_string(),
        }
    }

    /// Check cost against thresholds. Returns newly-crossed threshold (if any)
    /// and fires a desktop notification. Each level fires at most once per day.
    pub fn check(&mut self, current_cost: f64, settings: &Settings) -> Option<ThresholdLevel> {
        let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
        if today != self.current_date {
            self.fired.clear();
            self.current_date = today;
        }

        let level = if current_cost >= settings.daily_cost_critical {
            Some(ThresholdLevel::Critical)
        } else if current_cost >= settings.daily_cost_warn {
            Some(ThresholdLevel::Warn)
        } else {
            None
        };

        let level = level?;
        if self.fired.contains(&level) {
            return None;
        }

        self.fired.insert(level);

        // Fire desktop notification (best effort, requires "tray" feature)
        #[cfg(feature = "tray")]
        {
            let (title, urgency) = match level {
                ThresholdLevel::Warn => (
                    format!("Claude Code usage warning: ${:.2} today", current_cost),
                    notify_rust::Urgency::Normal,
                ),
                ThresholdLevel::Critical => (
                    format!("Claude Code usage CRITICAL: ${:.2} today", current_cost),
                    notify_rust::Urgency::Critical,
                ),
            };

            let threshold = match level {
                ThresholdLevel::Warn => settings.daily_cost_warn,
                ThresholdLevel::Critical => settings.daily_cost_critical,
            };

            let _ = notify_rust::Notification::new()
                .summary(&title)
                .body(&format!("Threshold: ${:.2}", threshold))
                .urgency(urgency)
                .timeout(notify_rust::Timeout::Milliseconds(8000))
                .show();
        }

        Some(level)
    }
}

/// Color for the cost label based on alert state.
pub fn cost_color(cost: f64, settings: &Settings) -> eframe::egui::Color32 {
    if cost >= settings.daily_cost_critical {
        eframe::egui::Color32::from_rgb(255, 80, 60) // red
    } else if cost >= settings.daily_cost_warn {
        eframe::egui::Color32::from_rgb(255, 200, 50) // yellow
    } else {
        eframe::egui::Color32::from_rgb(255, 180, 50) // default orange
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_alert_state_new() {
        let state = AlertState::new();
        assert!(state.fired.is_empty());
    }

    #[test]
    fn test_no_alert_below_warn() {
        let mut state = AlertState::new();
        let settings = Settings::default();
        assert!(state.check(5.0, &settings).is_none());
    }

    #[test]
    fn test_warn_fires_once() {
        let mut state = AlertState::new();
        let settings = Settings::default();
        let first = state.check(settings.daily_cost_warn + 1.0, &settings);
        assert_eq!(first, Some(ThresholdLevel::Warn));

        // Second call should not fire again
        let second = state.check(settings.daily_cost_warn + 2.0, &settings);
        assert!(second.is_none());
    }

    #[test]
    fn test_critical_fires_after_warn() {
        let mut state = AlertState::new();
        let settings = Settings::default();
        let _ = state.check(settings.daily_cost_warn + 1.0, &settings); // fires warn
        let crit = state.check(settings.daily_cost_critical + 1.0, &settings);
        assert_eq!(crit, Some(ThresholdLevel::Critical));
    }

    #[test]
    fn test_cost_color_thresholds() {
        let settings = Settings::default();

        let green_ish = cost_color(5.0, &settings);
        assert_eq!(green_ish, eframe::egui::Color32::from_rgb(255, 180, 50));

        let yellow = cost_color(settings.daily_cost_warn + 1.0, &settings);
        assert_eq!(yellow, eframe::egui::Color32::from_rgb(255, 200, 50));

        let red = cost_color(settings.daily_cost_critical + 1.0, &settings);
        assert_eq!(red, eframe::egui::Color32::from_rgb(255, 80, 60));
    }
}
