//! Health check mode — run all analyzers once, print a summary, exit with a
//! severity-based code (FR-13).
//!
//! Exit codes:
//! - **0** — all analyzers found no issues (healthy)
//! - **1** — at least one Warning-level finding, no Critical findings
//! - **2** — at least one Critical-level finding

use tokio_postgres::Client;

use crate::governance::Severity;

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Run every available analyzer against `client`, print a human-readable
/// summary to stdout, and return an exit code.
///
/// - `0` — no findings
/// - `1` — warnings only
/// - `2` — at least one critical finding
pub async fn run_health_check(client: &Client) -> i32 {
    let mut total_warnings: usize = 0;
    let mut total_criticals: usize = 0;
    let mut analyzer_count: usize = 0;

    // -----------------------------------------------------------------
    // index_health
    // -----------------------------------------------------------------
    {
        let report = crate::index_health::analyze(client).await;
        analyzer_count += 1;
        let (w, c) = count_findings(&report.findings, |f| f.severity);
        print_analyzer_line("index_health", report.findings.len(), w, c);
        total_warnings += w;
        total_criticals += c;
    }

    // -----------------------------------------------------------------
    // vacuum
    // -----------------------------------------------------------------
    {
        let report = crate::vacuum::analyze(client).await;
        analyzer_count += 1;
        let (w, c) = count_findings(&report.findings, |f| f.severity);
        print_analyzer_line("vacuum", report.findings.len(), w, c);
        total_warnings += w;
        total_criticals += c;
    }

    // -----------------------------------------------------------------
    // bloat
    // -----------------------------------------------------------------
    {
        let report = crate::bloat::BloatAnalyzer::analyze(client).await;
        analyzer_count += 1;
        let (w, c) = count_findings(&report.findings, |f| f.severity);
        print_analyzer_line("bloat", report.findings.len(), w, c);
        total_warnings += w;
        total_criticals += c;
    }

    // -----------------------------------------------------------------
    // query_optimization
    // -----------------------------------------------------------------
    {
        let report = crate::query_optimization::analyze(client).await;
        analyzer_count += 1;
        let (w, c) = count_findings(&report.findings, |f| f.severity);
        print_analyzer_line("query_optimization", report.findings.len(), w, c);
        total_warnings += w;
        total_criticals += c;
    }

    // -----------------------------------------------------------------
    // config_tuning
    // -----------------------------------------------------------------
    {
        let report = crate::config_tuning::analyze(client).await;
        analyzer_count += 1;
        let (w, c) = count_findings(&report.findings, |f| f.severity);
        print_analyzer_line("config_tuning", report.findings.len(), w, c);
        total_warnings += w;
        total_criticals += c;
    }

    // -----------------------------------------------------------------
    // connection_management
    // -----------------------------------------------------------------
    {
        let report =
            crate::connection_management::ConnectionManagementAnalyzer::analyze(client).await;
        analyzer_count += 1;
        let (w, c) = count_findings(&report.findings, |f| f.severity);
        print_analyzer_line("connection_management", report.findings.len(), w, c);
        total_warnings += w;
        total_criticals += c;
    }

    // -----------------------------------------------------------------
    // replication
    // -----------------------------------------------------------------
    {
        let report = crate::replication::ReplicationAnalyzer::analyze(client).await;
        analyzer_count += 1;
        let (w, c) = count_findings(&report.findings, |f| f.severity);
        print_analyzer_line("replication", report.findings.len(), w, c);
        total_warnings += w;
        total_criticals += c;
    }

    // -----------------------------------------------------------------
    // backup_monitoring
    // -----------------------------------------------------------------
    {
        let report = crate::backup_monitoring::BackupMonitoringAnalyzer::analyze(client).await;
        analyzer_count += 1;
        let (w, c) = count_findings(&report.findings, |f| f.severity);
        print_analyzer_line("backup_monitoring", report.findings.len(), w, c);
        total_warnings += w;
        total_criticals += c;
    }

    // -----------------------------------------------------------------
    // security
    // -----------------------------------------------------------------
    {
        let report = crate::security::SecurityAnalyzer::analyze(client).await;
        analyzer_count += 1;
        let (w, c) = count_findings(&report.findings, |f| f.severity);
        print_analyzer_line("security", report.findings.len(), w, c);
        total_warnings += w;
        total_criticals += c;
    }

    // -----------------------------------------------------------------
    // Summary line
    // -----------------------------------------------------------------
    println!();
    if total_criticals > 0 {
        println!(
            "CRITICAL — {total_criticals} critical, {total_warnings} warning(s) \
             ({analyzer_count} analyzers checked)"
        );
        2
    } else if total_warnings > 0 {
        println!(
            "WARNING — {total_warnings} warning(s) \
             ({analyzer_count} analyzers checked)"
        );
        1
    } else {
        println!("OK — no issues found ({analyzer_count} analyzers checked)");
        0
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Count warnings and criticals in a slice of findings, extracting the
/// severity via `get_severity`.
fn count_findings<T, F>(findings: &[T], get_severity: F) -> (usize, usize)
where
    F: Fn(&T) -> Severity,
{
    let warnings = findings
        .iter()
        .filter(|f| get_severity(f) == Severity::Warning)
        .count();
    let criticals = findings
        .iter()
        .filter(|f| get_severity(f) == Severity::Critical)
        .count();
    (warnings, criticals)
}

/// Print a one-line status for a single analyzer.
fn print_analyzer_line(name: &str, total: usize, warnings: usize, criticals: usize) {
    if total == 0 {
        println!("  {name}: ok");
    } else {
        let mut parts: Vec<String> = Vec::new();
        if criticals > 0 {
            parts.push(format!("{criticals} critical"));
        }
        if warnings > 0 {
            parts.push(format!("{warnings} warning(s)"));
        }
        let info = total - warnings - criticals;
        if info > 0 {
            parts.push(format!("{info} info"));
        }
        println!("  {name}: {}", parts.join(", "));
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn count_findings_empty() {
        let findings: Vec<Severity> = vec![];
        let (w, c) = count_findings(&findings, |s| *s);
        assert_eq!(w, 0);
        assert_eq!(c, 0);
    }

    #[test]
    fn count_findings_mixed() {
        let findings = vec![
            Severity::Info,
            Severity::Warning,
            Severity::Critical,
            Severity::Warning,
        ];
        let (w, c) = count_findings(&findings, |s| *s);
        assert_eq!(w, 2);
        assert_eq!(c, 1);
    }

    #[test]
    fn count_findings_all_ok() {
        let findings = vec![Severity::Info, Severity::Info];
        let (w, c) = count_findings(&findings, |s| *s);
        assert_eq!(w, 0);
        assert_eq!(c, 0);
    }
}
