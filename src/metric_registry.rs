/// Classification of how a metric is produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetricClass {
    /// Directly observed from API responses or tool invocations.
    Measured,
    /// Computed from measured values via deterministic formula.
    Derived,
    /// Heuristic-based signal; formula may change between versions.
    Inferred,
}

/// A registered metric definition with classification and version info.
pub struct MetricDef {
    pub id: &'static str,
    pub display_name: &'static str,
    pub class: MetricClass,
    /// 0 for Measured/Derived, >= 1 for Inferred (tracks formula changes).
    pub version: u32,
    /// Formula or source description (shown on hover).
    pub description: &'static str,
}

pub const METRICS: &[MetricDef] = &[
    // ── Measured ──
    MetricDef {
        id: "input_tokens",
        display_name: "Input Tokens",
        class: MetricClass::Measured,
        version: 0,
        description: "Raw input token count from API",
    },
    MetricDef {
        id: "output_tokens",
        display_name: "Output Tokens",
        class: MetricClass::Measured,
        version: 0,
        description: "Raw output token count from API",
    },
    MetricDef {
        id: "cache_creation_tokens",
        display_name: "Cache Creation Tokens",
        class: MetricClass::Measured,
        version: 0,
        description: "Cache creation tokens from API",
    },
    MetricDef {
        id: "cache_read_tokens",
        display_name: "Cache Read Tokens",
        class: MetricClass::Measured,
        version: 0,
        description: "Cache read tokens from API",
    },
    MetricDef {
        id: "tool_calls",
        display_name: "Tool Calls",
        class: MetricClass::Measured,
        version: 0,
        description: "Tool invocation counts per name",
    },
    MetricDef {
        id: "tool_latency",
        display_name: "Tool Latency",
        class: MetricClass::Measured,
        version: 0,
        description: "Correlated from tool_use/tool_result timestamps",
    },
    MetricDef {
        id: "tool_errors",
        display_name: "Tool Errors",
        class: MetricClass::Measured,
        version: 0,
        description: "is_tool_error from tool results",
    },
    MetricDef {
        id: "file_touches",
        display_name: "File Touches",
        class: MetricClass::Measured,
        version: 0,
        description: "Per-file read/write/edit/grep counts",
    },
    MetricDef {
        id: "subagent_spawns",
        display_name: "Subagent Spawns",
        class: MetricClass::Measured,
        version: 0,
        description: "Agent tool invocations",
    },
    // ── Derived ──
    MetricDef {
        id: "burn_rate",
        display_name: "Burn Rate",
        class: MetricClass::Derived,
        version: 0,
        description: "output_tokens / window_minutes",
    },
    MetricDef {
        id: "cache_efficiency",
        display_name: "Cache Efficiency",
        class: MetricClass::Derived,
        version: 0,
        description: "cache_read / (cache_read + input)",
    },
    MetricDef {
        id: "edit_precision",
        display_name: "Edit Precision",
        class: MetricClass::Derived,
        version: 0,
        description: "total_new_len / total_old_len",
    },
    MetricDef {
        id: "exploration_breadth",
        display_name: "Exploration Breadth",
        class: MetricClass::Derived,
        version: 0,
        description: "unique_files_searched.len()",
    },
    MetricDef {
        id: "avg_prompt_length",
        display_name: "Avg Prompt Length",
        class: MetricClass::Derived,
        version: 0,
        description: "sum(prompt_lengths) / count",
    },
    MetricDef {
        id: "tree_depth",
        display_name: "Tree Depth",
        class: MetricClass::Derived,
        version: 0,
        description: "BFS max depth through parent_to_children",
    },
    MetricDef {
        id: "cost_per_tool",
        display_name: "Cost per Tool",
        class: MetricClass::Derived,
        version: 0,
        description: "tokens * model price, apportioned",
    },
    MetricDef {
        id: "cost_per_1k_output",
        display_name: "Cost per 1K Output",
        class: MetricClass::Derived,
        version: 0,
        description: "total_cost / (output / 1000)",
    },
    MetricDef {
        id: "tool_cooccurrence",
        display_name: "Tool Co-occurrence",
        class: MetricClass::Derived,
        version: 0,
        description: "Pair counts from per-turn tool lists",
    },
    MetricDef {
        id: "tool_trigrams",
        display_name: "Tool Trigrams",
        class: MetricClass::Derived,
        version: 0,
        description: "3-consecutive-tool patterns",
    },
    MetricDef {
        id: "bash_categories",
        display_name: "Bash Categories",
        class: MetricClass::Derived,
        version: 0,
        description: "Regex classification of first word",
    },
    // ── Inferred ──
    MetricDef {
        id: "search_before_act",
        display_name: "Search-before-act",
        class: MetricClass::Inferred,
        version: 1,
        description: "search_ops / (search_ops + action_ops); thresholds >0.4/0.2",
    },
    MetricDef {
        id: "search_act_signal",
        display_name: "Search-Act Signal",
        class: MetricClass::Inferred,
        version: 1,
        description: "Experimental heuristic: min(search_act_ratio * 1.5, 1.0)",
    },
    MetricDef {
        id: "session_phase",
        display_name: "Session Phase",
        class: MetricClass::Inferred,
        version: 1,
        description: "Tool combo -> Explore/Plan/Implement/Verify",
    },
    MetricDef {
        id: "prompt_intent",
        display_name: "Prompt Intent",
        class: MetricClass::Inferred,
        version: 1,
        description: "text_length < 80 -> directive, >= 80 -> question",
    },
    MetricDef {
        id: "retry_detection",
        display_name: "Retry Detection",
        class: MetricClass::Inferred,
        version: 1,
        description: "Same tool+file within 120s",
    },
    MetricDef {
        id: "tdd_cycle",
        display_name: "TDD Cycle",
        class: MetricClass::Inferred,
        version: 1,
        description: "T-E-T pattern in rolling 20-tool window",
    },
    MetricDef {
        id: "token_waste",
        display_name: "Token Waste",
        class: MetricClass::Inferred,
        version: 1,
        description: "input > 50K && output < 100",
    },
    MetricDef {
        id: "write_then_edit",
        display_name: "Write-then-Edit",
        class: MetricClass::Inferred,
        version: 1,
        description: "Edit on file already written this session",
    },
];

/// Look up a metric definition by id.
pub fn lookup(id: &str) -> Option<&'static MetricDef> {
    METRICS.iter().find(|m| m.id == id)
}

/// Iterate over all inferred metrics.
pub fn inferred_metrics() -> impl Iterator<Item = &'static MetricDef> {
    METRICS.iter().filter(|m| m.class == MetricClass::Inferred)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_all_ids_unique() {
        let mut seen = std::collections::HashSet::new();
        for m in METRICS {
            assert!(seen.insert(m.id), "Duplicate metric id: {}", m.id);
        }
    }

    #[test]
    fn test_measured_derived_version_zero() {
        for m in METRICS {
            if m.class != MetricClass::Inferred {
                assert_eq!(m.version, 0, "Non-inferred metric {} should have version 0", m.id);
            }
        }
    }

    #[test]
    fn test_inferred_version_nonzero() {
        for m in inferred_metrics() {
            assert!(m.version >= 1, "Inferred metric {} should have version >= 1", m.id);
        }
    }

    #[test]
    fn test_lookup() {
        assert!(lookup("input_tokens").is_some());
        assert!(lookup("search_act_signal").is_some());
        assert!(lookup("nonexistent").is_none());
    }

    #[test]
    fn test_inferred_count() {
        let count = inferred_metrics().count();
        assert_eq!(count, 8);
    }
}
