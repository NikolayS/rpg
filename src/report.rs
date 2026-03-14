//! Full diagnostic report mode — run all analyzers, produce detailed output,
//! exit with severity code (FR-13 extended).
//!
//! Exit codes:
//! - **0** — all analyzers found no issues (healthy)
//! - **1** — at least one Warning-level finding, no Critical findings
//! - **2** — at least one Critical-level finding

use tokio_postgres::Client;

use crate::governance::Severity;

const ANALYZER_COUNT: usize = 9;

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Run every available analyzer against `client`, print a detailed report to
/// stdout, and return an exit code.
///
/// `format` must be `"text"` (default) or `"json"`.
///
/// - `0` — no findings
/// - `1` — warnings only
/// - `2` — at least one critical finding
pub async fn run_report(client: &Client, format: &str) -> i32 {
    match format {
        "json" => run_report_json(client).await,
        _ => run_report_text(client).await,
    }
}

// ---------------------------------------------------------------------------
// Shared finding abstraction used by both output formats
// ---------------------------------------------------------------------------

/// Normalised view of a single finding used for rendering, independent of
/// which analyzer produced it.
struct NormFinding {
    kind: &'static str,
    description: String,
    severity: Severity,
    suggestion: Option<String>,
}

/// Aggregate result for one analyzer section.
struct AnalyzerResult {
    name: &'static str,
    findings: Vec<NormFinding>,
    warnings: usize,
    criticals: usize,
}

impl AnalyzerResult {
    fn new(name: &'static str, findings: Vec<NormFinding>) -> Self {
        let warnings = findings
            .iter()
            .filter(|f| f.severity == Severity::Warning)
            .count();
        let criticals = findings
            .iter()
            .filter(|f| f.severity == Severity::Critical)
            .count();
        Self {
            name,
            findings,
            warnings,
            criticals,
        }
    }

    fn is_clean(&self) -> bool {
        self.findings.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Collect all analyzer results
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_lines)]
async fn collect_results(client: &Client) -> Vec<AnalyzerResult> {
    let mut results = Vec::with_capacity(ANALYZER_COUNT);

    // index_health
    {
        let report = crate::index_health::analyze(client).await;
        let findings = report
            .findings
            .into_iter()
            .map(|f| NormFinding {
                kind: f.kind.label(),
                description: f.description,
                severity: f.severity,
                suggestion: f.suggested_action,
            })
            .collect();
        results.push(AnalyzerResult::new("index_health", findings));
    }

    // vacuum
    {
        let report = crate::vacuum::analyze(client).await;
        let findings = report
            .findings
            .into_iter()
            .map(|f| NormFinding {
                kind: f.kind.label(),
                description: f.description,
                severity: f.severity,
                suggestion: f.suggested_action,
            })
            .collect();
        results.push(AnalyzerResult::new("vacuum", findings));
    }

    // bloat
    {
        let report = crate::bloat::BloatAnalyzer::analyze(client).await;
        let findings = report
            .findings
            .into_iter()
            .map(|f| NormFinding {
                kind: f.kind.label(),
                description: f.description,
                severity: f.severity,
                suggestion: f.suggested_action,
            })
            .collect();
        results.push(AnalyzerResult::new("bloat", findings));
    }

    // query_optimization
    {
        let report = crate::query_optimization::analyze(client).await;
        let findings = report
            .findings
            .into_iter()
            .map(|f| NormFinding {
                kind: f.kind.label(),
                description: f.description,
                severity: f.severity,
                suggestion: f.suggested_action,
            })
            .collect();
        results.push(AnalyzerResult::new("query_optimization", findings));
    }

    // config_tuning
    {
        let report = crate::config_tuning::analyze(client).await;
        let findings = report
            .findings
            .into_iter()
            .map(|f| NormFinding {
                kind: f.kind.label(),
                description: f.description,
                severity: f.severity,
                suggestion: f.suggested_action,
            })
            .collect();
        results.push(AnalyzerResult::new("config_tuning", findings));
    }

    // connection_management
    {
        let report =
            crate::connection_management::ConnectionManagementAnalyzer::analyze(client).await;
        let findings = report
            .findings
            .into_iter()
            .map(|f| NormFinding {
                kind: f.kind.label(),
                description: f.description,
                severity: f.severity,
                suggestion: f.suggested_action,
            })
            .collect();
        results.push(AnalyzerResult::new("connection_management", findings));
    }

    // replication
    {
        let report = crate::replication::ReplicationAnalyzer::analyze(client).await;
        let findings = report
            .findings
            .into_iter()
            .map(|f| NormFinding {
                kind: f.kind.label(),
                description: f.description,
                severity: f.severity,
                suggestion: f.suggested_action,
            })
            .collect();
        results.push(AnalyzerResult::new("replication", findings));
    }

    // backup_monitoring
    {
        let report = crate::backup_monitoring::BackupMonitoringAnalyzer::analyze(client).await;
        let findings = report
            .findings
            .into_iter()
            .map(|f| NormFinding {
                kind: f.kind.label(),
                description: f.description,
                severity: f.severity,
                suggestion: f.suggested_action,
            })
            .collect();
        results.push(AnalyzerResult::new("backup_monitoring", findings));
    }

    // security
    {
        let report = crate::security::SecurityAnalyzer::analyze(client).await;
        let findings = report
            .findings
            .into_iter()
            .map(|f| NormFinding {
                kind: f.kind.label(),
                description: f.description,
                severity: f.severity,
                suggestion: f.suggested_action,
            })
            .collect();
        results.push(AnalyzerResult::new("security", findings));
    }

    results
}

// ---------------------------------------------------------------------------
// Text format
// ---------------------------------------------------------------------------

async fn run_report_text(client: &Client) -> i32 {
    let results = collect_results(client).await;

    let total_warnings: usize = results.iter().map(|r| r.warnings).sum();
    let total_criticals: usize = results.iter().map(|r| r.criticals).sum();
    let clean_count: usize = results.iter().filter(|r| r.is_clean()).count();

    println!("=== Rpg Health Report ===");

    for result in &results {
        println!();
        println!("--- {} ---", result.name);
        if result.findings.is_empty() {
            println!("  (no issues)");
        } else {
            for f in &result.findings {
                let icon = match f.severity {
                    Severity::Critical => "!!",
                    Severity::Warning => "! ",
                    Severity::Info => "  ",
                };
                println!("  {icon} [{}]", f.kind);
                println!("     {}", f.description);
                if let Some(s) = &f.suggestion {
                    println!("     suggestion: {s}");
                }
            }
        }
    }

    println!();
    println!("=== Summary ===");
    println!(
        "Analyzers: {ANALYZER_COUNT} | Critical: {total_criticals} \
         | Warnings: {total_warnings} | Clean: {clean_count}"
    );

    severity_exit_code(total_criticals, total_warnings)
}

// ---------------------------------------------------------------------------
// JSON format
// ---------------------------------------------------------------------------

async fn run_report_json(client: &Client) -> i32 {
    let results = collect_results(client).await;

    let total_warnings: usize = results.iter().map(|r| r.warnings).sum();
    let total_criticals: usize = results.iter().map(|r| r.criticals).sum();
    let clean_count: usize = results.iter().filter(|r| r.is_clean()).count();

    let mut analyzers = serde_json::Map::new();

    for result in results {
        let findings_json: Vec<serde_json::Value> = result
            .findings
            .iter()
            .map(|f| {
                serde_json::json!({
                    "severity": severity_label(f.severity),
                    "kind": f.kind,
                    "description": f.description,
                    "suggestion": f.suggestion,
                })
            })
            .collect();

        let status = if result.criticals > 0 {
            "critical"
        } else if result.warnings > 0 {
            "warning"
        } else {
            "clean"
        };

        analyzers.insert(
            result.name.to_string(),
            serde_json::json!({
                "status": status,
                "findings": findings_json,
            }),
        );
    }

    let output = serde_json::json!({
        "analyzers": analyzers,
        "summary": {
            "total": ANALYZER_COUNT,
            "critical": total_criticals,
            "warnings": total_warnings,
            "clean": clean_count,
        },
    });

    println!(
        "{}",
        serde_json::to_string_pretty(&output)
            .unwrap_or_else(|e| { format!("{{\"error\": \"{e}\"}}") })
    );

    severity_exit_code(total_criticals, total_warnings)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Map severity to a lowercase string for JSON output.
fn severity_label(s: Severity) -> &'static str {
    match s {
        Severity::Critical => "critical",
        Severity::Warning => "warning",
        Severity::Info => "info",
    }
}

/// Return the appropriate exit code given finding counts.
fn severity_exit_code(criticals: usize, warnings: usize) -> i32 {
    if criticals > 0 {
        return 2;
    }
    i32::from(warnings > 0)
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn severity_exit_code_clean() {
        assert_eq!(severity_exit_code(0, 0), 0);
    }

    #[test]
    fn severity_exit_code_warning() {
        assert_eq!(severity_exit_code(0, 3), 1);
    }

    #[test]
    fn severity_exit_code_critical() {
        assert_eq!(severity_exit_code(2, 1), 2);
    }

    #[test]
    fn severity_label_values() {
        assert_eq!(severity_label(Severity::Critical), "critical");
        assert_eq!(severity_label(Severity::Warning), "warning");
        assert_eq!(severity_label(Severity::Info), "info");
    }

    #[test]
    fn analyzer_result_counts() {
        let findings = vec![
            NormFinding {
                kind: "test_kind",
                description: "desc".to_string(),
                severity: Severity::Warning,
                suggestion: None,
            },
            NormFinding {
                kind: "test_kind",
                description: "desc".to_string(),
                severity: Severity::Critical,
                suggestion: None,
            },
            NormFinding {
                kind: "test_kind",
                description: "desc".to_string(),
                severity: Severity::Info,
                suggestion: None,
            },
        ];
        let result = AnalyzerResult::new("test", findings);
        assert_eq!(result.warnings, 1);
        assert_eq!(result.criticals, 1);
        assert!(!result.is_clean());
    }

    #[test]
    fn analyzer_result_clean() {
        let result = AnalyzerResult::new("test", vec![]);
        assert_eq!(result.warnings, 0);
        assert_eq!(result.criticals, 0);
        assert!(result.is_clean());
    }
}
