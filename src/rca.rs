//! Root Cause Analysis (RCA) — LLM-driven investigation chain.
//!
//! Collects diagnostic data from `pg_stat_activity`, `pg_locks`,
//! `pg_stat_statements`, and optionally `pg_ash`, then sends findings
//! to an LLM for interpretation and three-tier mitigation recommendations.
//!
//! The investigation chain follows 8 steps (SPEC §8.5):
//! 1. Big picture (activity summary)
//! 2. Wait breakdown
//! 3. Timeline (`pg_ash` only)
//! 4. Query attribution
//! 5. Query deep-dive
//! 6. Lock analysis (block tree reconstruction)
//! 7. Stat correlation (`pg_stat_statements`)
//! 8. Object state (table/index stats)

use std::fmt::Write as _;

use crate::governance::{EvidenceClass, FeatureArea, Severity};

// ---------------------------------------------------------------------------
// Investigation step results
// ---------------------------------------------------------------------------

/// A single step result in the RCA investigation chain.
#[derive(Debug, Clone)]
pub struct StepResult {
    /// Step number (1-8).
    pub step: u8,
    /// Human-readable step name.
    pub name: &'static str,
    /// Raw query output from the database.
    pub data: String,
    /// Whether this step had meaningful data.
    pub has_data: bool,
}

/// The complete diagnostic snapshot collected by the investigation chain.
#[derive(Debug, Clone, Default)]
pub struct DiagnosticSnapshot {
    /// Results from each investigation step.
    pub steps: Vec<StepResult>,
    /// Whether `pg_ash` was available for enhanced data collection.
    pub pg_ash_available: bool,
}

impl DiagnosticSnapshot {
    /// Format all collected data into a single prompt for the LLM.
    pub fn to_prompt(&self) -> String {
        let mut prompt = String::new();
        let mode = if self.pg_ash_available {
            "Full (pg_ash available)"
        } else {
            "Degraded (pg_stat_activity only — no historical data)"
        };
        let _ = writeln!(prompt, "=== RCA Diagnostic Snapshot ===");
        let _ = writeln!(prompt, "Mode: {mode}\n");

        for step in &self.steps {
            let _ = writeln!(prompt, "--- Step {}: {} ---", step.step, step.name);
            if step.has_data {
                let _ = writeln!(prompt, "{}", step.data);
            } else {
                let _ = writeln!(prompt, "(no data — step skipped or empty result)");
            }
            prompt.push('\n');
        }

        prompt
    }
}

// ---------------------------------------------------------------------------
// RCA finding (output)
// ---------------------------------------------------------------------------

/// A structured RCA finding with three-tier mitigation.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct RcaFinding {
    /// Brief title of the finding.
    pub title: String,
    /// Detailed description of the root cause.
    pub description: String,
    /// Evidence classification for governance decisions.
    pub evidence_class: EvidenceClass,
    /// Severity of the issue.
    pub severity: Severity,
    /// Confidence percentage (0-100).
    pub confidence: u8,
    /// Immediate mitigation (cancel/terminate, ANALYZE).
    pub immediate: Vec<String>,
    /// Mid-term mitigation (GUC tuning, index creation).
    pub mid_term: Vec<String>,
    /// Long-term mitigation (app changes, schema redesign).
    pub long_term: Vec<String>,
}

impl RcaFinding {
    /// Feature area this finding belongs to.
    #[allow(clippy::unused_self, dead_code)]
    pub fn feature_area(&self) -> FeatureArea {
        FeatureArea::Rca
    }
}

/// A complete RCA report.
#[allow(dead_code)]
#[derive(Debug, Clone, Default)]
pub struct RcaReport {
    /// Findings from the investigation.
    pub findings: Vec<RcaFinding>,
    /// Raw LLM analysis text.
    pub raw_analysis: String,
    /// The diagnostic snapshot that was analyzed.
    pub snapshot: DiagnosticSnapshot,
}

impl RcaReport {
    /// Format the report for terminal display.
    #[allow(dead_code)]
    pub fn display(&self) -> String {
        let mut out = String::new();

        if self.findings.is_empty() {
            out.push_str("No significant findings.\n");
            return out;
        }

        for (i, f) in self.findings.iter().enumerate() {
            let _ = writeln!(
                out,
                "\n{icon} Finding {num}: {title}",
                icon = severity_icon(f.severity),
                num = i + 1,
                title = f.title,
            );
            let _ = writeln!(
                out,
                "   Confidence: {conf}% | Evidence: {ev:?} | Severity: {sev:?}",
                conf = f.confidence,
                ev = f.evidence_class,
                sev = f.severity,
            );
            let _ = writeln!(out, "   {desc}", desc = f.description);

            if !f.immediate.is_empty() {
                let _ = writeln!(out, "\n   Immediate:");
                for action in &f.immediate {
                    let _ = writeln!(out, "     - {action}");
                }
            }
            if !f.mid_term.is_empty() {
                let _ = writeln!(out, "   Mid-term:");
                for action in &f.mid_term {
                    let _ = writeln!(out, "     - {action}");
                }
            }
            if !f.long_term.is_empty() {
                let _ = writeln!(out, "   Long-term:");
                for action in &f.long_term {
                    let _ = writeln!(out, "     - {action}");
                }
            }
        }

        out
    }
}

#[allow(dead_code)]
fn severity_icon(s: Severity) -> &'static str {
    match s {
        Severity::Info => "[i]",
        Severity::Warning => "[!]",
        Severity::Critical => "[!!]",
    }
}

// ---------------------------------------------------------------------------
// Diagnostic queries (block tree + activity)
// ---------------------------------------------------------------------------

/// SQL: recursive block tree reconstruction (SPEC Appendix E.10).
pub const BLOCK_TREE_SQL: &str = r"
WITH RECURSIVE lock_tree AS (
  SELECT
    pid,
    ARRAY[]::integer[] AS blocked_by,
    query,
    state,
    wait_event_type,
    wait_event,
    now() - state_change AS holding_duration,
    0 AS depth,
    ARRAY[pid] AS path
  FROM pg_stat_activity
  WHERE cardinality(pg_blocking_pids(pid)) = 0
    AND pid != pg_backend_pid()
    AND pid IN (
      SELECT DISTINCT unnest(pg_blocking_pids(pid))
      FROM pg_stat_activity
      WHERE cardinality(pg_blocking_pids(pid)) > 0
    )

  UNION ALL

  SELECT
    sa.pid,
    pg_blocking_pids(sa.pid),
    sa.query,
    sa.state,
    sa.wait_event_type,
    sa.wait_event,
    now() - sa.state_change,
    lt.depth + 1,
    lt.path || sa.pid
  FROM pg_stat_activity sa
  JOIN lock_tree lt ON lt.pid = ANY(pg_blocking_pids(sa.pid))
  WHERE NOT sa.pid = ANY(lt.path)
    AND lt.depth < 10
)
SELECT
  repeat('  ', depth) || pid::text AS pid_tree,
  depth,
  left(query, 80) AS query_preview,
  state,
  coalesce(wait_event_type || ':' || wait_event, '') AS wait,
  holding_duration::text
FROM lock_tree
ORDER BY path
";

/// SQL: activity summary (step 1 — degraded mode).
pub const ACTIVITY_SUMMARY_SQL: &str = r"
SELECT
  state,
  count(*) AS count,
  coalesce(wait_event_type, 'CPU/Running') AS wait_type,
  count(*) FILTER (WHERE now() - query_start > interval '5 seconds') AS slow
FROM pg_stat_activity
WHERE pid != pg_backend_pid()
  AND backend_type = 'client backend'
GROUP BY state, wait_event_type
ORDER BY count DESC
";

/// SQL: wait breakdown (step 2 — degraded mode).
pub const WAIT_BREAKDOWN_SQL: &str = r"
SELECT
  coalesce(wait_event_type, 'CPU/Running') AS wait_type,
  coalesce(wait_event, 'active') AS wait_event,
  count(*) AS sessions,
  count(*) FILTER (WHERE state = 'active') AS active
FROM pg_stat_activity
WHERE pid != pg_backend_pid()
  AND backend_type = 'client backend'
GROUP BY wait_event_type, wait_event
ORDER BY sessions DESC
LIMIT 20
";

/// SQL: top queries by time (step 4 — from `pg_stat_statements`).
pub const TOP_QUERIES_SQL: &str = r"
SELECT
  queryid,
  calls,
  round(total_exec_time::numeric, 2) AS total_ms,
  round(mean_exec_time::numeric, 2) AS mean_ms,
  rows,
  left(query, 100) AS query_preview
FROM pg_stat_statements
WHERE userid = (SELECT usesysid FROM pg_user WHERE usename = current_user)
ORDER BY total_exec_time DESC
LIMIT 10
";

/// SQL: table stats (step 8 — object state).
pub const OBJECT_STATE_SQL: &str = r"
SELECT
  schemaname || '.' || relname AS table_name,
  n_live_tup AS live_rows,
  n_dead_tup AS dead_rows,
  CASE WHEN n_live_tup > 0
    THEN round(100.0 * n_dead_tup / n_live_tup, 1)
    ELSE 0
  END AS dead_pct,
  last_vacuum::text,
  last_autovacuum::text,
  last_analyze::text,
  last_autoanalyze::text
FROM pg_stat_user_tables
WHERE n_dead_tup > 1000
   OR (n_live_tup > 0 AND n_dead_tup::float / n_live_tup > 0.1)
ORDER BY n_dead_tup DESC
LIMIT 20
";

// ---------------------------------------------------------------------------
// Data collector
// ---------------------------------------------------------------------------

/// Collect a diagnostic snapshot from the database.
///
/// Runs the investigation chain queries and packages results. Skips
/// `pg_ash`-dependent steps when `pg_ash` is not available.
pub async fn collect_snapshot(
    client: &tokio_postgres::Client,
    pg_ash_available: bool,
) -> DiagnosticSnapshot {
    let mut snapshot = DiagnosticSnapshot {
        pg_ash_available,
        ..Default::default()
    };

    // Step 1: Activity summary
    snapshot
        .steps
        .push(run_step(client, 1, "Activity summary", ACTIVITY_SUMMARY_SQL).await);

    // Step 2: Wait breakdown
    snapshot
        .steps
        .push(run_step(client, 2, "Wait breakdown", WAIT_BREAKDOWN_SQL).await);

    // Steps 3-5: pg_ash-dependent (skipped in degraded mode)
    if pg_ash_available {
        snapshot.steps.push(StepResult {
            step: 3,
            name: "Timeline (pg_ash)",
            data: String::new(),
            has_data: false,
        });
        snapshot.steps.push(StepResult {
            step: 4,
            name: "Query attribution (pg_ash)",
            data: String::new(),
            has_data: false,
        });
        snapshot.steps.push(StepResult {
            step: 5,
            name: "Query deep-dive (pg_ash)",
            data: String::new(),
            has_data: false,
        });
    } else {
        for (step, name) in [
            (3, "Timeline (requires pg_ash)"),
            (4, "Query attribution"),
            (5, "Query deep-dive"),
        ] {
            snapshot.steps.push(StepResult {
                step,
                name,
                data: "Skipped: pg_ash not available".to_owned(),
                has_data: false,
            });
        }
    }

    // Step 4 fallback: top queries from pg_stat_statements
    if !pg_ash_available {
        snapshot.steps.push(
            run_step(
                client,
                4,
                "Top queries (pg_stat_statements)",
                TOP_QUERIES_SQL,
            )
            .await,
        );
    }

    // Step 6: Block tree
    snapshot
        .steps
        .push(run_step(client, 6, "Lock analysis (block tree)", BLOCK_TREE_SQL).await);

    // Step 7: pg_stat_statements correlation (reuse top queries if available)
    if pg_ash_available {
        snapshot.steps.push(
            run_step(
                client,
                7,
                "Stat correlation (pg_stat_statements)",
                TOP_QUERIES_SQL,
            )
            .await,
        );
    }

    // Step 8: Object state
    snapshot
        .steps
        .push(run_step(client, 8, "Object state (tables)", OBJECT_STATE_SQL).await);

    snapshot
}

/// Execute a single diagnostic query and package the result.
async fn run_step(
    client: &tokio_postgres::Client,
    step: u8,
    name: &'static str,
    sql: &str,
) -> StepResult {
    match client.simple_query(sql).await {
        Ok(msgs) => {
            let data = format_simple_query_result(&msgs);
            let has_data = !data.trim().is_empty();
            StepResult {
                step,
                name,
                data,
                has_data,
            }
        }
        Err(e) => StepResult {
            step,
            name,
            data: format!("Error: {e}"),
            has_data: false,
        },
    }
}

/// Format `simple_query` results into a readable table string.
fn format_simple_query_result(msgs: &[tokio_postgres::SimpleQueryMessage]) -> String {
    let mut columns: Vec<String> = Vec::new();
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut first_row = true;

    for msg in msgs {
        if let tokio_postgres::SimpleQueryMessage::Row(row) = msg {
            if first_row {
                // Extract column names from the first row.
                for i in 0..row.columns().len() {
                    columns.push(row.columns()[i].name().to_owned());
                }
                first_row = false;
            }
            let mut r = Vec::new();
            for i in 0..row.columns().len() {
                r.push(row.get(i).unwrap_or("NULL").to_owned());
            }
            rows.push(r);
        }
    }

    if columns.is_empty() {
        return String::new();
    }

    let mut out = String::new();
    let _ = writeln!(out, "{}", columns.join(" | "));
    let _ = writeln!(out, "{}", "-".repeat(columns.join(" | ").len()));
    for row in &rows {
        let _ = writeln!(out, "{}", row.join(" | "));
    }
    let _ = writeln!(out, "({} rows)", rows.len());

    out
}

// ---------------------------------------------------------------------------
// LLM prompt for RCA
// ---------------------------------------------------------------------------

/// Build the system prompt for the RCA LLM analysis.
pub fn rca_system_prompt(schema_context: &str) -> String {
    format!(
        "You are an expert PostgreSQL DBA performing root cause analysis.\n\
         \n\
         You are given a diagnostic snapshot from a PostgreSQL database. Analyze the data\n\
         and identify the root cause(s) of any performance issues.\n\
         \n\
         For each finding, provide:\n\
         1. A clear title\n\
         2. A description of the root cause\n\
         3. Confidence level (High >80%, Medium 40-80%, Low <40%)\n\
         4. Three-tier mitigation:\n\
         \x20  - Immediate: Actions to resolve the issue now (cancel/terminate, ANALYZE, etc.)\n\
         \x20  - Mid-term: Configuration changes (GUC tuning, index creation)\n\
         \x20  - Long-term: Application or schema changes\n\
         \n\
         Focus on actionable findings. Do not speculate without evidence.\n\
         \n\
         If no issues are found, say so clearly.\n\
         \n\
         Database schema:\n\
         {schema_context}\n\
         \n\
         Respond in plain text with clear section headers."
    )
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostic_snapshot_default() {
        let snap = DiagnosticSnapshot::default();
        assert!(snap.steps.is_empty());
        assert!(!snap.pg_ash_available);
    }

    #[test]
    fn diagnostic_snapshot_to_prompt_empty() {
        let snap = DiagnosticSnapshot::default();
        let prompt = snap.to_prompt();
        assert!(prompt.contains("Degraded"));
        assert!(prompt.contains("RCA Diagnostic Snapshot"));
    }

    #[test]
    fn diagnostic_snapshot_to_prompt_with_data() {
        let snap = DiagnosticSnapshot {
            pg_ash_available: true,
            steps: vec![StepResult {
                step: 1,
                name: "Activity summary",
                data: "active | 5".to_owned(),
                has_data: true,
            }],
        };
        let prompt = snap.to_prompt();
        assert!(prompt.contains("Full (pg_ash available)"));
        assert!(prompt.contains("Step 1: Activity summary"));
        assert!(prompt.contains("active | 5"));
    }

    #[test]
    fn diagnostic_snapshot_skipped_step() {
        let snap = DiagnosticSnapshot {
            pg_ash_available: false,
            steps: vec![StepResult {
                step: 3,
                name: "Timeline (requires pg_ash)",
                data: String::new(),
                has_data: false,
            }],
        };
        let prompt = snap.to_prompt();
        assert!(prompt.contains("no data"));
    }

    #[test]
    fn rca_finding_feature_area() {
        let f = RcaFinding {
            title: "test".to_owned(),
            description: "desc".to_owned(),
            evidence_class: EvidenceClass::Factual,
            severity: Severity::Warning,
            confidence: 80,
            immediate: vec![],
            mid_term: vec![],
            long_term: vec![],
        };
        assert_eq!(f.feature_area(), FeatureArea::Rca);
    }

    #[test]
    fn rca_report_display_no_findings() {
        let report = RcaReport::default();
        let out = report.display();
        assert!(out.contains("No significant findings"));
    }

    #[test]
    fn rca_report_display_with_findings() {
        let report = RcaReport {
            findings: vec![RcaFinding {
                title: "Lock contention".to_owned(),
                description: "PID 123 blocking 5 sessions".to_owned(),
                evidence_class: EvidenceClass::Factual,
                severity: Severity::Critical,
                confidence: 95,
                immediate: vec!["Cancel PID 123".to_owned()],
                mid_term: vec!["Set lock_timeout = '5s'".to_owned()],
                long_term: vec!["Reduce transaction scope".to_owned()],
            }],
            raw_analysis: String::new(),
            snapshot: DiagnosticSnapshot::default(),
        };
        let out = report.display();
        assert!(out.contains("Lock contention"));
        assert!(out.contains("[!!]"));
        assert!(out.contains("95%"));
        assert!(out.contains("Cancel PID 123"));
        assert!(out.contains("lock_timeout"));
        assert!(out.contains("Reduce transaction scope"));
    }

    #[test]
    fn severity_icons() {
        assert_eq!(severity_icon(Severity::Info), "[i]");
        assert_eq!(severity_icon(Severity::Warning), "[!]");
        assert_eq!(severity_icon(Severity::Critical), "[!!]");
    }

    #[test]
    fn format_empty_result() {
        let msgs: Vec<tokio_postgres::SimpleQueryMessage> = vec![];
        let out = format_simple_query_result(&msgs);
        assert!(out.is_empty());
    }

    #[test]
    fn rca_system_prompt_contains_schema() {
        let prompt = rca_system_prompt("CREATE TABLE users (id bigint);");
        assert!(prompt.contains("CREATE TABLE users"));
        assert!(prompt.contains("root cause analysis"));
        assert!(prompt.contains("Three-tier mitigation"));
    }

    #[test]
    fn block_tree_sql_is_valid_structure() {
        assert!(BLOCK_TREE_SQL.contains("WITH RECURSIVE lock_tree"));
        assert!(BLOCK_TREE_SQL.contains("pg_blocking_pids"));
        assert!(BLOCK_TREE_SQL.contains("depth < 10"));
    }

    #[test]
    fn activity_summary_sql_excludes_self() {
        assert!(ACTIVITY_SUMMARY_SQL.contains("pg_backend_pid()"));
    }
}
