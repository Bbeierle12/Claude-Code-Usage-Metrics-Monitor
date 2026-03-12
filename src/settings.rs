use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Subscription plan tier, determines default usage limits.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub enum PlanTier {
    Pro,
    Max5x,
    Max20x,
}

impl PlanTier {
    pub const ALL: [PlanTier; 3] = [PlanTier::Pro, PlanTier::Max5x, PlanTier::Max20x];

    pub fn label(self) -> &'static str {
        match self {
            PlanTier::Pro => "Pro",
            PlanTier::Max5x => "Max 5x",
            PlanTier::Max20x => "Max 20x",
        }
    }

    /// Default output token limits per 5-hour window for each tier.
    /// Returns (opus, sonnet, haiku).
    pub fn default_limits(self) -> (u64, u64, u64) {
        match self {
            PlanTier::Pro => (100_000, 400_000, 800_000),
            PlanTier::Max5x => (500_000, 2_000_000, 4_000_000),
            PlanTier::Max20x => (2_000_000, 8_000_000, 16_000_000),
        }
    }
}

impl Default for PlanTier {
    fn default() -> Self {
        PlanTier::Pro
    }
}

/// Per-model pricing in USD per million tokens.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelPricing {
    pub input_per_m: f64,
    pub output_per_m: f64,
    pub cache_write_per_m: f64,
    pub cache_read_per_m: f64,
}

/// Validation errors collected by `Settings::validate()`.
#[derive(Debug, Clone, Default)]
pub struct ValidationErrors {
    pub errors: Vec<String>,
}

impl ValidationErrors {
    pub fn is_empty(&self) -> bool {
        self.errors.is_empty()
    }
}

/// Runtime-editable settings with JSON persistence.
/// All fields default to the same values as `config.rs` constants.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Settings {
    // Alert thresholds (USD)
    pub daily_cost_warn: f64,
    pub daily_cost_critical: f64,

    // Burn rate
    pub burn_rate_window_minutes: i64,
    pub burn_rate_low: f64,
    pub burn_rate_high: f64,

    // Session timing
    pub active_session_threshold_minutes: i64,
    /// Gaps longer than this between consecutive messages are counted as idle.
    pub idle_gap_minutes: i64,

    // Window size
    pub window_width: f32,
    pub window_height: f32,

    // Model pricing
    pub opus_pricing: ModelPricing,
    pub sonnet_pricing: ModelPricing,
    pub haiku_pricing: ModelPricing,

    // Plan usage limits
    pub plan_tier: PlanTier,
    pub usage_window_hours: f64,
    pub opus_output_limit: u64,
    pub sonnet_output_limit: u64,
    pub haiku_output_limit: u64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            daily_cost_warn: 10.0,
            daily_cost_critical: 25.0,
            burn_rate_window_minutes: 10,
            burn_rate_low: 500.0,
            burn_rate_high: 2000.0,
            active_session_threshold_minutes: 5,
            idle_gap_minutes: 5,
            window_width: 600.0,
            window_height: 720.0,
            opus_pricing: ModelPricing {
                input_per_m: 15.0,
                output_per_m: 75.0,
                cache_write_per_m: 18.75,
                cache_read_per_m: 1.50,
            },
            sonnet_pricing: ModelPricing {
                input_per_m: 3.0,
                output_per_m: 15.0,
                cache_write_per_m: 3.75,
                cache_read_per_m: 0.30,
            },
            haiku_pricing: ModelPricing {
                input_per_m: 0.80,
                output_per_m: 4.0,
                cache_write_per_m: 1.0,
                cache_read_per_m: 0.08,
            },
            plan_tier: PlanTier::default(),
            usage_window_hours: 5.0,
            opus_output_limit: 100_000,
            sonnet_output_limit: 400_000,
            haiku_output_limit: 800_000,
        }
    }
}

impl Settings {
    /// Returns (input_per_m, output_per_m, cache_write_per_m, cache_read_per_m) for a model.
    /// Unknown models default to Sonnet pricing with a one-time stderr warning.
    pub fn cost_rates(&self, model: &str) -> (f64, f64, f64, f64) {
        let p = if model.contains("opus") {
            &self.opus_pricing
        } else if model.contains("haiku") {
            &self.haiku_pricing
        } else if model.contains("sonnet") || model.is_empty() || model == "unknown" {
            &self.sonnet_pricing
        } else {
            // Unknown model — log once and fall back to Sonnet pricing
            use std::sync::Mutex;
            static WARNED: Mutex<Vec<String>> = Mutex::new(Vec::new());
            if let Ok(mut warned) = WARNED.lock() {
                if !warned.iter().any(|w| w == model) {
                    eprintln!("Warning: unknown model '{}', using Sonnet pricing", model);
                    warned.push(model.to_string());
                }
            }
            &self.sonnet_pricing
        };
        (p.input_per_m, p.output_per_m, p.cache_write_per_m, p.cache_read_per_m)
    }

    /// Estimate cost in USD for a set of token counts at the given model's rates.
    pub fn estimate_cost(
        &self,
        model: &str,
        input: u64,
        output: u64,
        cache_creation: u64,
        cache_read: u64,
    ) -> f64 {
        let (inp_rate, out_rate, cw_rate, cr_rate) = self.cost_rates(model);
        (input as f64 * inp_rate
            + output as f64 * out_rate
            + cache_creation as f64 * cw_rate
            + cache_read as f64 * cr_rate)
            / 1_000_000.0
    }

    /// Returns the output token limit for a model within the usage window.
    pub fn output_limit_for_model(&self, model: &str) -> u64 {
        if model.contains("opus") {
            self.opus_output_limit
        } else if model.contains("haiku") {
            self.haiku_output_limit
        } else {
            self.sonnet_output_limit
        }
    }

    /// Apply default limits from the current plan tier.
    pub fn apply_tier_defaults(&mut self) {
        let (opus, sonnet, haiku) = self.plan_tier.default_limits();
        self.opus_output_limit = opus;
        self.sonnet_output_limit = sonnet;
        self.haiku_output_limit = haiku;
    }

    /// Validate settings. Returns errors for invalid combinations.
    pub fn validate(&self) -> ValidationErrors {
        let mut errors = ValidationErrors::default();

        if self.daily_cost_warn >= self.daily_cost_critical {
            errors
                .errors
                .push("Warning threshold must be less than critical".to_string());
        }
        if self.burn_rate_low >= self.burn_rate_high {
            errors
                .errors
                .push("Burn rate low must be less than high".to_string());
        }
        if self.active_session_threshold_minutes < 1 {
            errors
                .errors
                .push("Active session threshold must be >= 1 minute".to_string());
        }
        if self.burn_rate_window_minutes < 1 {
            errors
                .errors
                .push("Burn rate window must be >= 1 minute".to_string());
        }

        // Pricing must be non-negative
        for (name, p) in [
            ("Opus", &self.opus_pricing),
            ("Sonnet", &self.sonnet_pricing),
            ("Haiku", &self.haiku_pricing),
        ] {
            if p.input_per_m < 0.0
                || p.output_per_m < 0.0
                || p.cache_write_per_m < 0.0
                || p.cache_read_per_m < 0.0
            {
                errors
                    .errors
                    .push(format!("{} pricing values must be >= 0", name));
            }
        }

        // Window size minimums
        if self.window_width < 200.0 || self.window_height < 200.0 {
            errors
                .errors
                .push("Window size must be at least 200x200".to_string());
        }

        errors
    }

    /// Path to the settings JSON file (platform-correct config directory).
    pub fn path() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".config"))
            .join("claude-usage-card/settings.json")
    }

    /// Load settings from disk, falling back to defaults.
    /// Logs a warning if the settings file exists but contains invalid JSON.
    pub fn load() -> Self {
        let path = Self::path();
        match std::fs::read_to_string(&path) {
            Ok(contents) => match serde_json::from_str(&contents) {
                Ok(settings) => settings,
                Err(e) => {
                    eprintln!(
                        "Warning: invalid settings at {}, using defaults: {}",
                        path.display(),
                        e
                    );
                    Self::default()
                }
            },
            Err(_) => Self::default(),
        }
    }

    /// Save settings to disk as pretty JSON.
    pub fn save(&self) -> Result<(), String> {
        let path = Self::path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create config dir: {}", e))?;
        }
        let json =
            serde_json::to_string_pretty(self).map_err(|e| format!("Serialize error: {}", e))?;
        std::fs::write(&path, json).map_err(|e| format!("Write error: {}", e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_valid() {
        let s = Settings::default();
        assert!(s.validate().is_empty());
    }

    #[test]
    fn validate_warn_gte_critical() {
        let mut s = Settings::default();
        s.daily_cost_warn = 30.0;
        s.daily_cost_critical = 25.0;
        assert!(!s.validate().is_empty());
    }

    #[test]
    fn validate_burn_rate() {
        let mut s = Settings::default();
        s.burn_rate_low = 3000.0;
        s.burn_rate_high = 2000.0;
        assert!(!s.validate().is_empty());
    }

    #[test]
    fn validate_negative_pricing() {
        let mut s = Settings::default();
        s.opus_pricing.input_per_m = -1.0;
        assert!(!s.validate().is_empty());
    }

    #[test]
    fn validate_zero_threshold() {
        let mut s = Settings::default();
        s.active_session_threshold_minutes = 0;
        assert!(!s.validate().is_empty());
    }

    #[test]
    fn validate_small_window() {
        let mut s = Settings::default();
        s.window_width = 100.0;
        assert!(!s.validate().is_empty());
    }

    #[test]
    fn estimate_cost_sonnet() {
        let s = Settings::default();
        let cost = s.estimate_cost("sonnet", 1_000_000, 0, 0, 0);
        assert!((cost - 3.0).abs() < 0.001);
    }

    #[test]
    fn estimate_cost_opus() {
        let s = Settings::default();
        let cost = s.estimate_cost("opus", 0, 1_000_000, 0, 0);
        assert!((cost - 75.0).abs() < 0.001);
    }

    #[test]
    fn cost_rates_unknown() {
        let s = Settings::default();
        let (inp, _, _, _) = s.cost_rates("unknown-model");
        // Should default to sonnet-tier
        assert!((inp - 3.0).abs() < 0.001);
    }

    #[test]
    fn roundtrip_serialize() {
        let s = Settings::default();
        let json = serde_json::to_string(&s).unwrap();
        let s2: Settings = serde_json::from_str(&json).unwrap();
        assert_eq!(s, s2);
    }

    #[test]
    fn load_missing_file() {
        // When file doesn't exist, should return defaults
        let s = Settings::load();
        assert_eq!(s, Settings::default());
    }
}
