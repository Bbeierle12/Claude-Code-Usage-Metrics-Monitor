/// Session is "active" if it had activity within this many minutes.
pub const ACTIVE_SESSION_THRESHOLD_MINUTES: i64 = 5;

/// Cost per million tokens by model (USD). Approximations as of early 2026.
/// Format: (input_per_m, output_per_m, cache_write_per_m, cache_read_per_m)
pub fn cost_rates(model: &str) -> (f64, f64, f64, f64) {
    match model {
        m if m.contains("opus") => (15.0, 75.0, 18.75, 1.50),
        m if m.contains("sonnet") => (3.0, 15.0, 3.75, 0.30),
        m if m.contains("haiku") => (0.80, 4.0, 1.0, 0.08),
        _ => (3.0, 15.0, 3.75, 0.30), // default to sonnet-tier
    }
}

/// Estimate cost in USD for a set of token counts at the given model's rates.
pub fn estimate_cost(
    model: &str,
    input: u64,
    output: u64,
    cache_creation: u64,
    cache_read: u64,
) -> f64 {
    let (inp_rate, out_rate, cw_rate, cr_rate) = cost_rates(model);
    (input as f64 * inp_rate
        + output as f64 * out_rate
        + cache_creation as f64 * cw_rate
        + cache_read as f64 * cr_rate)
        / 1_000_000.0
}

/// Window size for the egui card.
pub const WINDOW_WIDTH: f32 = 520.0;
pub const WINDOW_HEIGHT: f32 = 600.0;

/// Subdirectory under home where Claude Code stores projects.
pub const CLAUDE_PROJECTS_REL: &str = ".claude/projects";
