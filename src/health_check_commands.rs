//! CLI command handlers for health check management.
//!
//! Provides formatting and management functions for [`HealthCheckRegistry`]
//! entries. Wired into the REPL via `\health` backslash commands.
//!
//! ## Limitation
//!
//! The [`HealthCheckRegistry`] public API exposes only `list_enabled()` and
//! `list_by_feature()` — it has no `list_all()`. As a result,
//! `format_health_list` shows only enabled checks. Disabled checks become
//! visible again once the registry gains a public `list_all()` iterator.

use std::fs;
use std::path::Path;

use crate::health_checks::{HealthCheckDefinition, HealthCheckRegistry};

// ---------------------------------------------------------------------------
// Formatting helpers
// ---------------------------------------------------------------------------

/// Column widths used by the list table.
const COL_NAME: usize = 25;
const COL_FEATURE: usize = 22;
const COL_SEVERITY: usize = 9;
const COL_AUTONOMY: usize = 11;
const COL_ENABLED: usize = 7;

/// Total separator width: sum of columns + single space between each.
const SEPARATOR_WIDTH: usize =
    COL_NAME + 1 + COL_FEATURE + 1 + COL_SEVERITY + 1 + COL_AUTONOMY + 1 + COL_ENABLED;

/// Format all registered health checks as a table.
///
/// Columns: Name, Feature, Severity, Autonomy, Enabled.
/// The output is sorted by name for deterministic display.
///
/// Returns an empty header-only table when the registry contains no enabled
/// checks.
///
/// Note: only enabled checks are shown; the registry does not yet expose a
/// `list_all()` iterator.
pub fn format_health_list(registry: &HealthCheckRegistry) -> String {
    let header = format!(
        "{:<width_name$} {:<width_feat$} {:<width_sev$} {:<width_auto$} {}",
        "Name",
        "Feature",
        "Severity",
        "Autonomy",
        "Enabled",
        width_name = COL_NAME,
        width_feat = COL_FEATURE,
        width_sev = COL_SEVERITY,
        width_auto = COL_AUTONOMY,
    );
    let separator = "\u{2500}".repeat(SEPARATOR_WIDTH);

    let mut checks: Vec<&HealthCheckDefinition> = registry.list_enabled();
    checks.sort_by(|a, b| a.name.cmp(&b.name));

    if checks.is_empty() {
        return format!("{header}\n{separator}\n");
    }

    let mut rows = vec![header, separator];
    for check in checks {
        let enabled = if check.enabled { "yes" } else { "no" };
        rows.push(format!(
            "{:<width_name$} {:<width_feat$} {:<width_sev$} {:<width_auto$} {}",
            check.name,
            check.feature_area,
            check.severity,
            check.max_autonomy,
            enabled,
            width_name = COL_NAME,
            width_feat = COL_FEATURE,
            width_sev = COL_SEVERITY,
            width_auto = COL_AUTONOMY,
        ));
    }
    rows.join("\n") + "\n"
}

/// Format full details of a single named health check.
///
/// Returns a multi-line string with every field displayed, suitable for
/// a `\health show <name>` style command.
///
/// Returns `"Health check not found: {name}"` when the name is unknown.
pub fn format_health_show(registry: &HealthCheckRegistry, name: &str) -> String {
    let Some(check) = registry.get(name) else {
        return format!("Health check not found: {name}");
    };

    let action = check.proposed_action.as_deref().unwrap_or("(none)");
    let tags = if check.tags.is_empty() {
        "(none)".to_owned()
    } else {
        check.tags.join(", ")
    };
    let enabled = if check.enabled { "yes" } else { "no" };

    format!(
        "Name:            {name}\n\
         Description:     {description}\n\
         Version:         {version}\n\
         Feature area:    {feature_area}\n\
         Evidence class:  {evidence_class}\n\
         Severity:        {severity}\n\
         Max autonomy:    {max_autonomy}\n\
         Enabled:         {enabled}\n\
         Tags:            {tags}\n\
         Proposed action: {action}\n\
         Detect SQL:\n\
         {detect_sql}\n",
        name = check.name,
        description = check.description,
        version = check.version,
        feature_area = check.feature_area,
        evidence_class = check.evidence_class,
        severity = check.severity,
        max_autonomy = check.max_autonomy,
        enabled = enabled,
        tags = tags,
        action = action,
        detect_sql = indent_sql(&check.detect_sql),
    )
}

/// Indent each line of a SQL string with four spaces for display purposes.
fn indent_sql(sql: &str) -> String {
    sql.lines()
        .map(|line| format!("    {line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

// ---------------------------------------------------------------------------
// TOML directory loader
// ---------------------------------------------------------------------------

/// Read all `.toml` files from `path`, parse each as a check file, register
/// the contained checks, and return the total count of newly-loaded checks.
///
/// Files that fail to parse are skipped and their errors are collected.
/// If no files were successfully loaded and at least one parse error
/// occurred, the first error is returned.
///
/// # Errors
///
/// Returns an error string when `path` cannot be read as a directory or when
/// every TOML file in the directory fails to parse.
#[allow(dead_code)]
pub fn load_health_checks_from_dir(
    registry: &mut HealthCheckRegistry,
    path: &Path,
) -> Result<usize, String> {
    let entries = fs::read_dir(path).map_err(|e| {
        format!(
            "health_check_commands: cannot read dir {}: {e}",
            path.display()
        )
    })?;

    let mut loaded = 0usize;
    let mut first_error: Option<String> = None;

    for entry in entries.flatten() {
        let file_path = entry.path();
        if file_path.extension().and_then(|e| e.to_str()) != Some("toml") {
            continue;
        }

        let content = match fs::read_to_string(&file_path) {
            Ok(c) => c,
            Err(e) => {
                let msg = format!(
                    "health_check_commands: cannot read {}: {e}",
                    file_path.display()
                );
                if first_error.is_none() {
                    first_error = Some(msg);
                }
                continue;
            }
        };

        match HealthCheckRegistry::load_from_toml(&content) {
            Ok(checks) => {
                let count = checks.len();
                for check in checks {
                    registry.register(check);
                }
                loaded += count;
            }
            Err(e) => {
                let msg = format!(
                    "health_check_commands: parse error in {}: {e}",
                    file_path.display()
                );
                if first_error.is_none() {
                    first_error = Some(msg);
                }
            }
        }
    }

    if loaded == 0 {
        if let Some(err) = first_error {
            return Err(err);
        }
    }

    Ok(loaded)
}

// ---------------------------------------------------------------------------
// Toggle helper
// ---------------------------------------------------------------------------

/// Enable or disable a health check by name.
///
/// Looks up the check in the registry, clones it, sets `enabled` to the
/// requested value, and re-registers the updated definition.
///
/// # Errors
///
/// Returns an error string when no check with the given name exists.
pub fn toggle_health_check(
    registry: &mut HealthCheckRegistry,
    name: &str,
    enabled: bool,
) -> Result<(), String> {
    let check = registry
        .get(name)
        .ok_or_else(|| format!("health_check_commands: check not found: {name}"))?
        .clone();

    registry.register(HealthCheckDefinition { enabled, ..check });
    Ok(())
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    fn minimal_check(name: &str) -> HealthCheckDefinition {
        HealthCheckDefinition {
            name: name.to_owned(),
            description: format!("Description for {name}"),
            version: "1.0.0".to_owned(),
            feature_area: "test_feature".to_owned(),
            detect_sql: "select 1".to_owned(),
            evidence_class: "factual".to_owned(),
            proposed_action: None,
            max_autonomy: "observe".to_owned(),
            tags: vec![],
            severity: "info".to_owned(),
            enabled: true,
        }
    }

    // -----------------------------------------------------------------------
    // format_health_list
    // -----------------------------------------------------------------------

    #[test]
    fn format_list_contains_header() {
        let registry = HealthCheckRegistry::new();
        let output = format_health_list(&registry);
        assert!(output.contains("Name"), "header should contain 'Name'");
        assert!(
            output.contains("Feature"),
            "header should contain 'Feature'"
        );
        assert!(
            output.contains("Severity"),
            "header should contain 'Severity'"
        );
        assert!(
            output.contains("Autonomy"),
            "header should contain 'Autonomy'"
        );
        assert!(
            output.contains("Enabled"),
            "header should contain 'Enabled'"
        );
    }

    #[test]
    fn format_list_contains_separator() {
        let registry = HealthCheckRegistry::new();
        let output = format_health_list(&registry);
        // The separator is composed of U+2500 box-drawing characters.
        assert!(
            output.contains('\u{2500}'),
            "output should contain a separator line"
        );
    }

    #[test]
    fn format_list_empty_registry_shows_header_only() {
        let registry = HealthCheckRegistry::new();
        let output = format_health_list(&registry);
        // Only the header + separator, no data rows.
        let lines: Vec<&str> = output.lines().collect();
        assert_eq!(lines.len(), 2, "empty registry should produce 2 lines");
    }

    #[test]
    fn format_list_default_registry_shows_all_checks() {
        let registry = HealthCheckRegistry::with_defaults();
        let output = format_health_list(&registry);
        assert!(output.contains("idle_xact_5min"));
        assert!(output.contains("table_bloat_50pct"));
        assert!(output.contains("unused_index_30d"));
        assert!(output.contains("long_running_query_5min"));
        assert!(output.contains("replication_lag_1min"));
    }

    #[test]
    fn format_list_shows_feature_and_severity() {
        let registry = HealthCheckRegistry::with_defaults();
        let output = format_health_list(&registry);
        assert!(output.contains("connection_management"));
        assert!(output.contains("warning"));
        assert!(output.contains("critical"));
    }

    #[test]
    fn format_list_shows_enabled_status() {
        let registry = HealthCheckRegistry::with_defaults();
        let output = format_health_list(&registry);
        assert!(output.contains("yes"), "all default checks are enabled");
    }

    #[test]
    fn format_list_sorted_by_name() {
        let mut registry = HealthCheckRegistry::new();
        registry.register(minimal_check("zzz_check"));
        registry.register(minimal_check("aaa_check"));
        registry.register(minimal_check("mmm_check"));

        let output = format_health_list(&registry);
        let aaa_pos = output.find("aaa_check").expect("aaa_check not found");
        let mmm_pos = output.find("mmm_check").expect("mmm_check not found");
        let zzz_pos = output.find("zzz_check").expect("zzz_check not found");
        assert!(aaa_pos < mmm_pos, "aaa should appear before mmm");
        assert!(mmm_pos < zzz_pos, "mmm should appear before zzz");
    }

    // -----------------------------------------------------------------------
    // format_health_show
    // -----------------------------------------------------------------------

    #[test]
    fn format_show_known_check_contains_all_fields() {
        let registry = HealthCheckRegistry::with_defaults();
        let output = format_health_show(&registry, "idle_xact_5min");
        assert!(output.contains("idle_xact_5min"), "should include name");
        assert!(
            output.contains("connection_management"),
            "should include feature_area"
        );
        assert!(output.contains("factual"), "should include evidence_class");
        assert!(output.contains("warning"), "should include severity");
        assert!(output.contains("supervised"), "should include max_autonomy");
        assert!(
            output.contains("pg_terminate_backend"),
            "should include proposed_action"
        );
        assert!(
            output.contains("pg_stat_activity"),
            "should include detect_sql fragment"
        );
    }

    #[test]
    fn format_show_unknown_check_returns_error_message() {
        let registry = HealthCheckRegistry::with_defaults();
        let output = format_health_show(&registry, "nonexistent_check");
        assert_eq!(output, "Health check not found: nonexistent_check");
    }

    #[test]
    fn format_show_check_without_action_shows_none() {
        let registry = HealthCheckRegistry::with_defaults();
        // replication_lag_1min has no proposed_action.
        let output = format_health_show(&registry, "replication_lag_1min");
        assert!(
            output.contains("(none)"),
            "missing action should show (none)"
        );
    }

    #[test]
    fn format_show_check_with_tags() {
        let registry = HealthCheckRegistry::with_defaults();
        let output = format_health_show(&registry, "idle_xact_5min");
        // idle_xact_5min has tags: idle, connection, transaction
        assert!(output.contains("idle"), "should list tags");
        assert!(output.contains("connection"), "should list tags");
    }

    #[test]
    fn format_show_check_without_tags_shows_none() {
        let mut registry = HealthCheckRegistry::new();
        let mut check = minimal_check("no_tags_check");
        check.tags = vec![];
        registry.register(check);
        let output = format_health_show(&registry, "no_tags_check");
        assert!(output.contains("(none)"), "empty tags should show (none)");
    }

    // -----------------------------------------------------------------------
    // load_health_checks_from_dir
    // -----------------------------------------------------------------------

    #[test]
    fn load_from_dir_reads_toml_files() {
        let dir = tempfile::tempdir().expect("failed to create temp dir");
        let toml_content = r#"
[[checks]]
name = "dir_check_a"
description = "Check A from dir"
version = "1.0.0"
feature_area = "vacuum"
detect_sql = "select 1"
evidence_class = "factual"
max_autonomy = "observe"
tags = []
severity = "info"
enabled = true
"#;
        let file_path = dir.path().join("checks.toml");
        let mut f = fs::File::create(&file_path).expect("create toml file");
        f.write_all(toml_content.as_bytes()).expect("write toml");

        let mut registry = HealthCheckRegistry::new();
        let count =
            load_health_checks_from_dir(&mut registry, dir.path()).expect("load should succeed");
        assert_eq!(count, 1, "should load 1 check");
        assert!(registry.get("dir_check_a").is_some());
    }

    #[test]
    fn load_from_dir_ignores_non_toml_files() {
        let dir = tempfile::tempdir().expect("failed to create temp dir");
        let json_path = dir.path().join("checks.json");
        let mut f = fs::File::create(&json_path).expect("create json file");
        f.write_all(b"{\"not\": \"toml\"}").expect("write json");

        let mut registry = HealthCheckRegistry::new();
        // Empty dir with only non-toml file: no error, 0 loaded.
        let count = load_health_checks_from_dir(&mut registry, dir.path())
            .expect("load with no toml files should return Ok(0)");
        assert_eq!(count, 0, "non-toml files should be ignored");
    }

    #[test]
    fn load_from_dir_multiple_files() {
        let dir = tempfile::tempdir().expect("failed to create temp dir");

        let make_toml = |name: &str| {
            format!(
                r#"
[[checks]]
name = "{name}"
description = "Check {name}"
version = "1.0.0"
feature_area = "bloat"
detect_sql = "select 1"
evidence_class = "heuristic"
max_autonomy = "supervised"
tags = []
severity = "warning"
enabled = true
"#
            )
        };

        for name in ["alpha", "beta"] {
            let path = dir.path().join(format!("{name}.toml"));
            let mut f = fs::File::create(&path).expect("create toml");
            f.write_all(make_toml(name).as_bytes()).expect("write");
        }

        let mut registry = HealthCheckRegistry::new();
        let count = load_health_checks_from_dir(&mut registry, dir.path()).expect("should succeed");
        assert_eq!(count, 2, "should load 2 checks from 2 files");
    }

    #[test]
    fn load_from_dir_bad_path_returns_error() {
        let mut registry = HealthCheckRegistry::new();
        let result = load_health_checks_from_dir(&mut registry, Path::new("/nonexistent/path/xyz"));
        assert!(result.is_err(), "bad path should return error");
        let msg = result.unwrap_err();
        assert!(
            msg.contains("health_check_commands:"),
            "error should include module prefix"
        );
    }

    // -----------------------------------------------------------------------
    // toggle_health_check
    // -----------------------------------------------------------------------

    #[test]
    fn toggle_enable_disables_check() {
        let mut registry = HealthCheckRegistry::with_defaults();
        // All defaults are enabled; disable one.
        toggle_health_check(&mut registry, "idle_xact_5min", false).expect("toggle should succeed");

        // list_enabled no longer contains it.
        let enabled_names: Vec<&str> = registry
            .list_enabled()
            .iter()
            .map(|c| c.name.as_str())
            .collect();
        assert!(
            !enabled_names.contains(&"idle_xact_5min"),
            "disabled check should not appear in list_enabled"
        );
    }

    #[test]
    fn toggle_re_enables_check() {
        let mut registry = HealthCheckRegistry::with_defaults();
        // Disable then re-enable.
        toggle_health_check(&mut registry, "idle_xact_5min", false).unwrap();
        toggle_health_check(&mut registry, "idle_xact_5min", true).unwrap();

        let enabled_names: Vec<&str> = registry
            .list_enabled()
            .iter()
            .map(|c| c.name.as_str())
            .collect();
        assert!(
            enabled_names.contains(&"idle_xact_5min"),
            "re-enabled check should appear in list_enabled"
        );
    }

    #[test]
    fn toggle_unknown_check_returns_error() {
        let mut registry = HealthCheckRegistry::with_defaults();
        let result = toggle_health_check(&mut registry, "nonexistent", true);
        assert!(result.is_err(), "unknown check should return error");
        let msg = result.unwrap_err();
        assert!(
            msg.contains("nonexistent"),
            "error should mention the missing name"
        );
    }

    #[test]
    fn toggle_preserves_other_fields() {
        let mut registry = HealthCheckRegistry::new();
        let mut check = minimal_check("preserve_check");
        check.tags = vec!["foo".to_owned(), "bar".to_owned()];
        check.severity = "critical".to_owned();
        registry.register(check);

        toggle_health_check(&mut registry, "preserve_check", false).unwrap();

        let updated = registry.get("preserve_check").unwrap();
        assert!(!updated.enabled, "enabled should be false");
        assert_eq!(updated.severity, "critical", "severity should be preserved");
        assert_eq!(updated.tags, vec!["foo", "bar"], "tags should be preserved");
    }
}
