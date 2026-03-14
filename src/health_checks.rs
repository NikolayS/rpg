//! Health check protocol schema — shareable, TOML-loadable check definitions.
//!
//! Provides a registry of [`HealthCheckDefinition`] structs that describe
//! individual database health checks. Checks can be defined in code (defaults)
//! or loaded from TOML files for distribution and customisation.
//!
//! # TOML format
//!
//! ```toml
//! [[checks]]
//! name = "idle_xact_5min"
//! description = "Detect idle-in-transaction sessions older than 5 minutes"
//! version = "1.0.0"
//! feature_area = "connection_management"
//! detect_sql = """
//! select pid
//! from pg_stat_activity
//! where
//!     state = 'idle in transaction'
//!     and now() - state_change > interval '5 minutes'
//! """
//! evidence_class = "factual"
//! proposed_action = "pg_terminate_backend($pid)"
//! max_autonomy = "supervised"
//! tags = ["idle", "connection"]
//! severity = "warning"
//! enabled = true
//! ```

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Core types
// ---------------------------------------------------------------------------

/// A shareable, serialisable definition of a single database health check.
///
/// Each check carries the SQL used for detection, its classification within
/// the AAA governance framework, and metadata for filtering and routing.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct HealthCheckDefinition {
    /// Unique machine-readable identifier (e.g. `idle_xact_5min`).
    pub name: String,
    /// Human-readable description of what the check detects.
    pub description: String,
    /// Semantic version of this check definition (e.g. `1.0.0`).
    pub version: String,
    /// Feature area this check belongs to (e.g. `connection_management`).
    pub feature_area: String,
    /// SQL query that returns rows when the problem is present.
    ///
    /// Should return an empty result set when everything is healthy.
    pub detect_sql: String,
    /// Evidence classification: `"factual"`, `"heuristic"`, or `"advisory"`.
    ///
    /// - `factual` — direct system-catalog evidence, high confidence
    /// - `heuristic` — statistical inference, medium confidence
    /// - `advisory` — best-practice recommendation, low confidence
    pub evidence_class: String,
    /// Optional action to take when the check fires.
    ///
    /// May contain `$variable` placeholders (e.g. `pg_terminate_backend($pid)`).
    pub proposed_action: Option<String>,
    /// Maximum autonomy level allowed: `"observe"`, `"supervised"`, or `"auto"`.
    pub max_autonomy: String,
    /// Free-form tags for grouping and filtering.
    pub tags: Vec<String>,
    /// Severity when the check fires: `"info"`, `"warning"`, or `"critical"`.
    pub severity: String,
    /// Whether the check is active. Disabled checks are skipped by the registry.
    pub enabled: bool,
}

// ---------------------------------------------------------------------------
// TOML envelope
// ---------------------------------------------------------------------------

/// Top-level TOML structure for loading a list of checks from a file.
///
/// Expects a document of the form:
///
/// ```toml
/// [[checks]]
/// name = "..."
/// ...
/// ```
#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct CheckFile {
    checks: Vec<HealthCheckDefinition>,
}

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

/// Registry of [`HealthCheckDefinition`] entries.
///
/// Supports loading from TOML, registering checks at runtime, and querying
/// by name, feature area, or enabled status.
pub struct HealthCheckRegistry {
    checks: Vec<HealthCheckDefinition>,
}

impl HealthCheckRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self { checks: Vec::new() }
    }

    /// Create a registry pre-populated with all built-in default checks.
    pub fn with_defaults() -> Self {
        let mut registry = Self::new();
        for check in default_checks() {
            registry.register(check);
        }
        registry
    }

    /// Parse a TOML string and return the contained check definitions.
    ///
    /// The TOML document must contain a `[[checks]]` array.
    ///
    /// # Errors
    ///
    /// Returns an error string if the TOML is malformed or does not match
    /// the expected schema.
    #[allow(dead_code)]
    pub fn load_from_toml(content: &str) -> Result<Vec<HealthCheckDefinition>, String> {
        toml::from_str::<CheckFile>(content)
            .map(|f| f.checks)
            .map_err(|e| format!("health_checks: TOML parse error: {e}"))
    }

    /// Add a single check to the registry.
    ///
    /// If a check with the same name already exists it is replaced.
    pub fn register(&mut self, check: HealthCheckDefinition) {
        if let Some(existing) = self.checks.iter_mut().find(|c| c.name == check.name) {
            *existing = check;
        } else {
            self.checks.push(check);
        }
    }

    /// Look up a check by its unique name.
    pub fn get(&self, name: &str) -> Option<&HealthCheckDefinition> {
        self.checks.iter().find(|c| c.name == name)
    }

    /// Return all checks belonging to a given feature area.
    #[allow(dead_code)]
    pub fn list_by_feature(&self, feature: &str) -> Vec<&HealthCheckDefinition> {
        self.checks
            .iter()
            .filter(|c| c.feature_area == feature)
            .collect()
    }

    /// Return all enabled checks.
    pub fn list_enabled(&self) -> Vec<&HealthCheckDefinition> {
        self.checks.iter().filter(|c| c.enabled).collect()
    }

    /// Return the total number of registered checks (enabled and disabled).
    pub fn len(&self) -> usize {
        self.checks.len()
    }

    /// Return `true` when the registry contains no checks.
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.checks.is_empty()
    }
}

impl Default for HealthCheckRegistry {
    fn default() -> Self {
        Self::with_defaults()
    }
}

// ---------------------------------------------------------------------------
// Built-in default checks
// ---------------------------------------------------------------------------

/// Return the canonical set of built-in health check definitions.
#[allow(clippy::too_many_lines)]
fn default_checks() -> Vec<HealthCheckDefinition> {
    vec![
        HealthCheckDefinition {
            name: "idle_xact_5min".to_owned(),
            description: "Detect idle-in-transaction sessions older than 5 minutes".to_owned(),
            version: "1.0.0".to_owned(),
            feature_area: "connection_management".to_owned(),
            detect_sql: "\
select
    pid,
    usename,
    application_name,
    now() - state_change as idle_duration
from pg_stat_activity
where
    state = 'idle in transaction'
    and now() - state_change > interval '5 minutes'\
"
            .to_owned(),
            evidence_class: "factual".to_owned(),
            proposed_action: Some("pg_terminate_backend($pid)".to_owned()),
            max_autonomy: "supervised".to_owned(),
            tags: vec![
                "idle".to_owned(),
                "connection".to_owned(),
                "transaction".to_owned(),
            ],
            severity: "warning".to_owned(),
            enabled: true,
        },
        HealthCheckDefinition {
            name: "table_bloat_50pct".to_owned(),
            description: "Detect tables with more than 50% dead-tuple bloat ratio".to_owned(),
            version: "1.0.0".to_owned(),
            feature_area: "bloat".to_owned(),
            detect_sql: "\
select
    schemaname,
    relname as table_name,
    n_dead_tup,
    n_live_tup,
    round(
        n_dead_tup::numeric / nullif(n_live_tup + n_dead_tup, 0) * 100,
        1
    ) as bloat_pct
from pg_stat_user_tables
where
    n_live_tup + n_dead_tup > 1000
    and n_dead_tup::numeric / nullif(n_live_tup + n_dead_tup, 0) > 0.5
order by bloat_pct desc\
"
            .to_owned(),
            evidence_class: "heuristic".to_owned(),
            proposed_action: Some("vacuum analyze $schemaname.$table_name".to_owned()),
            max_autonomy: "supervised".to_owned(),
            tags: vec!["bloat".to_owned(), "vacuum".to_owned(), "tables".to_owned()],
            severity: "warning".to_owned(),
            enabled: true,
        },
        HealthCheckDefinition {
            name: "unused_index_30d".to_owned(),
            description: "Detect indexes that have not been scanned in the last 30 days".to_owned(),
            version: "1.0.0".to_owned(),
            feature_area: "index_health".to_owned(),
            detect_sql: "\
select
    schemaname,
    relname as table_name,
    indexrelname as index_name,
    pg_size_pretty(pg_relation_size(indexrelid)) as index_size,
    idx_scan
from pg_stat_user_indexes
where
    idx_scan = 0
    and pg_relation_size(indexrelid) > 8192
order by pg_relation_size(indexrelid) desc\
"
            .to_owned(),
            evidence_class: "heuristic".to_owned(),
            proposed_action: Some("drop index concurrently $schemaname.$index_name".to_owned()),
            max_autonomy: "supervised".to_owned(),
            tags: vec!["index".to_owned(), "bloat".to_owned(), "unused".to_owned()],
            severity: "info".to_owned(),
            enabled: true,
        },
        HealthCheckDefinition {
            name: "long_running_query_5min".to_owned(),
            description: "Detect queries that have been running for more than 5 minutes".to_owned(),
            version: "1.0.0".to_owned(),
            feature_area: "query_optimization".to_owned(),
            detect_sql: "\
select
    pid,
    usename,
    application_name,
    state,
    now() - query_start as duration,
    left(query, 120) as query_snippet
from pg_stat_activity
where
    state != 'idle'
    and query_start is not null
    and now() - query_start > interval '5 minutes'
    and pid != pg_backend_pid()
order by duration desc\
"
            .to_owned(),
            evidence_class: "factual".to_owned(),
            proposed_action: Some("pg_cancel_backend($pid)".to_owned()),
            max_autonomy: "supervised".to_owned(),
            tags: vec![
                "queries".to_owned(),
                "performance".to_owned(),
                "long-running".to_owned(),
            ],
            severity: "warning".to_owned(),
            enabled: true,
        },
        HealthCheckDefinition {
            name: "replication_lag_1min".to_owned(),
            description: "Detect replication lag exceeding 1 minute on any standby".to_owned(),
            version: "1.0.0".to_owned(),
            feature_area: "replication".to_owned(),
            detect_sql: "\
select
    client_addr,
    application_name,
    state,
    write_lag,
    flush_lag,
    replay_lag
from pg_stat_replication
where
    replay_lag > interval '1 minute'
    or flush_lag > interval '1 minute'
order by replay_lag desc nulls last\
"
            .to_owned(),
            evidence_class: "factual".to_owned(),
            proposed_action: None,
            max_autonomy: "observe".to_owned(),
            tags: vec!["replication".to_owned(), "lag".to_owned(), "ha".to_owned()],
            severity: "critical".to_owned(),
            enabled: true,
        },
    ]
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // HealthCheckDefinition construction
    // -----------------------------------------------------------------------

    #[test]
    fn definition_fields_round_trip() {
        let def = HealthCheckDefinition {
            name: "test_check".to_owned(),
            description: "A test check".to_owned(),
            version: "1.0.0".to_owned(),
            feature_area: "vacuum".to_owned(),
            detect_sql: "select 1".to_owned(),
            evidence_class: "factual".to_owned(),
            proposed_action: Some("do_something()".to_owned()),
            max_autonomy: "auto".to_owned(),
            tags: vec!["test".to_owned()],
            severity: "info".to_owned(),
            enabled: true,
        };
        assert_eq!(def.name, "test_check");
        assert_eq!(def.evidence_class, "factual");
        assert!(def.proposed_action.is_some());
        assert!(def.enabled);
    }

    #[test]
    fn definition_optional_action_none() {
        let def = HealthCheckDefinition {
            name: "no_action".to_owned(),
            description: "Check without action".to_owned(),
            version: "1.0.0".to_owned(),
            feature_area: "replication".to_owned(),
            detect_sql: "select 1".to_owned(),
            evidence_class: "advisory".to_owned(),
            proposed_action: None,
            max_autonomy: "observe".to_owned(),
            tags: vec![],
            severity: "info".to_owned(),
            enabled: true,
        };
        assert!(def.proposed_action.is_none());
    }

    // -----------------------------------------------------------------------
    // TOML parsing
    // -----------------------------------------------------------------------

    #[test]
    fn load_from_toml_single_check() {
        let toml = r#"
[[checks]]
name = "idle_xact_5min"
description = "Detect idle-in-transaction sessions older than 5 minutes"
version = "1.0.0"
feature_area = "connection_management"
detect_sql = "select pid from pg_stat_activity where state = 'idle in transaction'"
evidence_class = "factual"
proposed_action = "pg_terminate_backend($pid)"
max_autonomy = "supervised"
tags = ["idle", "connection"]
severity = "warning"
enabled = true
"#;
        let checks = HealthCheckRegistry::load_from_toml(toml).unwrap();
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].name, "idle_xact_5min");
        assert_eq!(checks[0].evidence_class, "factual");
        assert_eq!(checks[0].severity, "warning");
        assert!(checks[0].enabled);
    }

    #[test]
    fn load_from_toml_multiple_checks() {
        let toml = r#"
[[checks]]
name = "check_a"
description = "First check"
version = "1.0.0"
feature_area = "vacuum"
detect_sql = "select 1"
evidence_class = "factual"
max_autonomy = "observe"
tags = []
severity = "info"
enabled = true

[[checks]]
name = "check_b"
description = "Second check"
version = "2.0.0"
feature_area = "bloat"
detect_sql = "select 2"
evidence_class = "heuristic"
max_autonomy = "supervised"
tags = ["bloat"]
severity = "warning"
enabled = false
"#;
        let checks = HealthCheckRegistry::load_from_toml(toml).unwrap();
        assert_eq!(checks.len(), 2);
        assert_eq!(checks[0].name, "check_a");
        assert_eq!(checks[1].name, "check_b");
        assert!(!checks[1].enabled);
    }

    #[test]
    fn load_from_toml_optional_action_absent() {
        let toml = r#"
[[checks]]
name = "no_action_check"
description = "Check with no proposed action"
version = "1.0.0"
feature_area = "replication"
detect_sql = "select 1"
evidence_class = "advisory"
max_autonomy = "observe"
tags = []
severity = "info"
enabled = true
"#;
        let checks = HealthCheckRegistry::load_from_toml(toml).unwrap();
        assert_eq!(checks.len(), 1);
        assert!(checks[0].proposed_action.is_none());
    }

    #[test]
    fn load_from_toml_invalid_returns_error() {
        let bad_toml = "this is not valid toml ][[[";
        let result = HealthCheckRegistry::load_from_toml(bad_toml);
        assert!(result.is_err());
        let msg = result.unwrap_err();
        assert!(msg.contains("health_checks:"));
    }

    #[test]
    fn load_from_toml_missing_required_field_returns_error() {
        // `name` is required — omitting it should cause a parse error.
        let toml = r#"
[[checks]]
description = "Missing name field"
version = "1.0.0"
feature_area = "vacuum"
detect_sql = "select 1"
evidence_class = "factual"
max_autonomy = "observe"
tags = []
severity = "info"
enabled = true
"#;
        let result = HealthCheckRegistry::load_from_toml(toml);
        assert!(result.is_err());
    }

    // -----------------------------------------------------------------------
    // Registry operations
    // -----------------------------------------------------------------------

    #[test]
    fn registry_new_is_empty() {
        let registry = HealthCheckRegistry::new();
        assert!(registry.is_empty());
        assert_eq!(registry.len(), 0);
    }

    #[test]
    fn registry_register_and_get() {
        let mut registry = HealthCheckRegistry::new();
        let check = HealthCheckDefinition {
            name: "my_check".to_owned(),
            description: "desc".to_owned(),
            version: "1.0.0".to_owned(),
            feature_area: "vacuum".to_owned(),
            detect_sql: "select 1".to_owned(),
            evidence_class: "factual".to_owned(),
            proposed_action: None,
            max_autonomy: "observe".to_owned(),
            tags: vec![],
            severity: "info".to_owned(),
            enabled: true,
        };
        registry.register(check);
        assert_eq!(registry.len(), 1);
        let found = registry.get("my_check");
        assert!(found.is_some());
        assert_eq!(found.unwrap().name, "my_check");
    }

    #[test]
    fn registry_register_replaces_existing() {
        let mut registry = HealthCheckRegistry::new();
        let original = HealthCheckDefinition {
            name: "dup_check".to_owned(),
            description: "original".to_owned(),
            version: "1.0.0".to_owned(),
            feature_area: "vacuum".to_owned(),
            detect_sql: "select 1".to_owned(),
            evidence_class: "factual".to_owned(),
            proposed_action: None,
            max_autonomy: "observe".to_owned(),
            tags: vec![],
            severity: "info".to_owned(),
            enabled: true,
        };
        let updated = HealthCheckDefinition {
            name: "dup_check".to_owned(),
            description: "updated".to_owned(),
            version: "2.0.0".to_owned(),
            feature_area: "vacuum".to_owned(),
            detect_sql: "select 2".to_owned(),
            evidence_class: "heuristic".to_owned(),
            proposed_action: None,
            max_autonomy: "supervised".to_owned(),
            tags: vec![],
            severity: "warning".to_owned(),
            enabled: true,
        };
        registry.register(original);
        registry.register(updated);
        // Still only one entry.
        assert_eq!(registry.len(), 1);
        let check = registry.get("dup_check").unwrap();
        assert_eq!(check.description, "updated");
        assert_eq!(check.version, "2.0.0");
    }

    #[test]
    fn registry_get_unknown_returns_none() {
        let registry = HealthCheckRegistry::new();
        assert!(registry.get("nonexistent").is_none());
    }

    // -----------------------------------------------------------------------
    // Default checks
    // -----------------------------------------------------------------------

    #[test]
    fn with_defaults_has_five_checks() {
        let registry = HealthCheckRegistry::with_defaults();
        assert_eq!(registry.len(), 5);
    }

    #[test]
    fn with_defaults_all_enabled() {
        let registry = HealthCheckRegistry::with_defaults();
        assert_eq!(registry.list_enabled().len(), 5);
    }

    #[test]
    fn with_defaults_contains_required_checks() {
        let registry = HealthCheckRegistry::with_defaults();
        assert!(registry.get("idle_xact_5min").is_some());
        assert!(registry.get("table_bloat_50pct").is_some());
        assert!(registry.get("unused_index_30d").is_some());
        assert!(registry.get("long_running_query_5min").is_some());
        assert!(registry.get("replication_lag_1min").is_some());
    }

    // -----------------------------------------------------------------------
    // Filtering
    // -----------------------------------------------------------------------

    #[test]
    fn list_by_feature_filters_correctly() {
        let registry = HealthCheckRegistry::with_defaults();
        let conn = registry.list_by_feature("connection_management");
        assert_eq!(conn.len(), 1);
        assert_eq!(conn[0].name, "idle_xact_5min");

        let repl = registry.list_by_feature("replication");
        assert_eq!(repl.len(), 1);
        assert_eq!(repl[0].name, "replication_lag_1min");
    }

    #[test]
    fn list_by_feature_unknown_returns_empty() {
        let registry = HealthCheckRegistry::with_defaults();
        let result = registry.list_by_feature("nonexistent_feature");
        assert!(result.is_empty());
    }

    #[test]
    fn list_enabled_excludes_disabled() {
        let mut registry = HealthCheckRegistry::with_defaults();
        // Disable one check by re-registering with enabled=false.
        let mut check = registry.get("idle_xact_5min").unwrap().clone();
        check.enabled = false;
        registry.register(check);

        let enabled = registry.list_enabled();
        assert_eq!(enabled.len(), 4);
        assert!(!enabled.iter().any(|c| c.name == "idle_xact_5min"));
    }
}
