//! Vacuum health Analyzer — detects dead-tuple bloat, XID wraparound risk,
//! stale tables, and autovacuum worker saturation.
//!
//! Operates at Observe level: reads `pg_stat_user_tables`, `pg_class`, and
//! `pg_stat_activity` to produce structured findings. No writes are performed.
//!
//! # Sub-findings
//!
//! | Sub-finding | Evidence Class | Source |
//! |---|---|---|
//! | High dead tuple ratio | Heuristic | `pg_stat_user_tables` |
//! | XID wraparound risk | Heuristic | `pg_class.relfrozenxid` age |
//! | Stale table (no vacuum in >7 days) | Heuristic | `pg_stat_user_tables` |
//! | Autovacuum worker count | Factual | `pg_stat_activity` |

use crate::governance::{EvidenceClass, Severity};

use std::fmt::Write as _;

// ---------------------------------------------------------------------------
// Vacuum finding types
// ---------------------------------------------------------------------------

/// Category of vacuum health finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VacuumFindingKind {
    /// Dead tuple ratio exceeds 10% of live tuples.
    HighDeadTuples,
    /// Table XID age exceeds 50% of `autovacuum_freeze_max_age`.
    XidWraparoundRisk,
    /// Table has not been vacuumed in more than 7 days.
    StaleTable,
    /// Current count of running autovacuum workers (informational).
    AutovacuumWorkerCount,
}

impl VacuumFindingKind {
    /// Evidence class for this finding kind.
    #[allow(dead_code)]
    pub fn evidence_class(self) -> EvidenceClass {
        match self {
            Self::AutovacuumWorkerCount => EvidenceClass::Factual,
            Self::HighDeadTuples | Self::XidWraparoundRisk | Self::StaleTable => {
                EvidenceClass::Heuristic
            }
        }
    }

    /// Human-readable label.
    pub fn label(self) -> &'static str {
        match self {
            Self::HighDeadTuples => "high_dead_tuples",
            Self::XidWraparoundRisk => "xid_wraparound_risk",
            Self::StaleTable => "stale_table",
            Self::AutovacuumWorkerCount => "autovacuum_workers",
        }
    }
}

/// A single vacuum health finding.
#[derive(Debug, Clone)]
pub struct VacuumFinding {
    /// What kind of finding.
    pub kind: VacuumFindingKind,
    /// Schema name (empty for instance-level findings).
    pub schema: String,
    /// Table name (empty for instance-level findings).
    pub table: String,
    /// Human-readable description.
    pub description: String,
    /// Severity level.
    pub severity: Severity,
    /// Evidence class.
    #[allow(dead_code)]
    pub evidence_class: EvidenceClass,
    /// Suggested remediation (Observe mode: informational only).
    pub suggested_action: Option<String>,
}

/// Complete vacuum health report.
#[derive(Debug, Clone)]
pub struct VacuumReport {
    /// All findings, sorted by severity (critical first).
    pub findings: Vec<VacuumFinding>,
}

impl VacuumReport {
    /// Display the report to the terminal.
    pub fn display(&self) {
        if self.findings.is_empty() {
            eprintln!("Vacuum health: no issues found.");
            return;
        }
        eprintln!(
            "Vacuum health: {} issue{} found.\n",
            self.findings.len(),
            if self.findings.len() == 1 { "" } else { "s" }
        );
        for f in &self.findings {
            let icon = match f.severity {
                Severity::Critical => "!!",
                Severity::Warning => "! ",
                Severity::Info => "  ",
            };
            if f.schema.is_empty() {
                eprintln!("{icon} [{}] {}", f.kind.label(), f.description);
            } else {
                eprintln!("{icon} [{}] {}.{}", f.kind.label(), f.schema, f.table,);
                eprintln!("   {}", f.description);
            }
            if let Some(ref action) = f.suggested_action {
                eprintln!("   suggestion: {action}");
            }
            eprintln!();
        }
    }

    /// Build a text summary for LLM consumption.
    #[allow(dead_code)]
    pub fn to_prompt(&self) -> String {
        if self.findings.is_empty() {
            return "No vacuum health issues found.".to_owned();
        }
        let mut out = format!(
            "Vacuum health report: {} finding(s)\n\n",
            self.findings.len()
        );
        for (i, f) in self.findings.iter().enumerate() {
            if f.schema.is_empty() {
                let _ = writeln!(out, "{}. [{}] {}", i + 1, f.kind.label(), f.description);
            } else {
                let _ = writeln!(
                    out,
                    "{}. [{}] {}.{}: {}",
                    i + 1,
                    f.kind.label(),
                    f.schema,
                    f.table,
                    f.description
                );
            }
            if let Some(ref action) = f.suggested_action {
                let _ = writeln!(out, "   Suggested: {action}");
            }
            out.push('\n');
        }
        out
    }
}

// ---------------------------------------------------------------------------
// SQL queries
// ---------------------------------------------------------------------------

/// Detect tables with high dead tuple counts (candidates for VACUUM).
///
/// Returns tables with > 1,000 dead tuples, ordered by dead tuple count.
const DEAD_TUPLES_SQL: &str = "\
    select \
        schemaname, \
        relname, \
        n_dead_tup, \
        n_live_tup, \
        last_vacuum, \
        last_autovacuum, \
        last_analyze \
    from pg_stat_user_tables \
    where n_dead_tup > 1000 \
    order by n_dead_tup desc \
    limit 20";

/// Detect tables approaching XID wraparound.
///
/// Returns tables where `age(relfrozenxid) > 50%` of `autovacuum_freeze_max_age`.
const XID_AGE_SQL: &str = "\
    select \
        c.oid::regclass as table_name, \
        age(c.relfrozenxid) as xid_age, \
        current_setting('autovacuum_freeze_max_age')::bigint as freeze_max \
    from pg_class as c \
    join pg_namespace as n \
        on c.relnamespace = n.oid \
    where \
        c.relkind = 'r' \
        and n.nspname not in ('pg_catalog', 'information_schema') \
    order by age(c.relfrozenxid) desc \
    limit 10";

/// Count running autovacuum worker processes.
const AUTOVACUUM_WORKERS_SQL: &str = "\
    select count(*) \
    from pg_stat_activity \
    where backend_type = 'autovacuum worker'";

// ---------------------------------------------------------------------------
// Public analyzer
// ---------------------------------------------------------------------------

/// Collect vacuum health findings from the database.
///
/// Runs diagnostic queries against `pg_catalog` and `pg_stat_*` views.
/// All operations are read-only (Observe mode).
pub async fn analyze(client: &tokio_postgres::Client) -> VacuumReport {
    let mut findings = Vec::new();

    collect_dead_tuple_findings(client, &mut findings).await;
    collect_xid_age_findings(client, &mut findings).await;
    collect_stale_table_findings(client, &mut findings).await;
    collect_autovacuum_worker_count(client, &mut findings).await;

    // Sort: Critical first, then Warning, then Info.
    findings.sort_by(|a, b| b.severity.cmp(&a.severity));

    VacuumReport { findings }
}

// ---------------------------------------------------------------------------
// Collection helpers
// ---------------------------------------------------------------------------

async fn collect_dead_tuple_findings(
    client: &tokio_postgres::Client,
    findings: &mut Vec<VacuumFinding>,
) {
    let Ok(messages) = client.simple_query(DEAD_TUPLES_SQL).await else {
        return;
    };
    for msg in messages {
        if let tokio_postgres::SimpleQueryMessage::Row(row) = msg {
            let schema = row.get(0).unwrap_or("").to_owned();
            let table = row.get(1).unwrap_or("").to_owned();
            let dead: i64 = row.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);
            let live: i64 = row.get(3).and_then(|s| s.parse().ok()).unwrap_or(0);

            let total = live + dead;
            // Safe precision loss: dead-tuple counts fit in 52-bit mantissa
            // for any realistic table size; percentage accuracy is sufficient.
            #[allow(clippy::cast_precision_loss)]
            let dead_pct = if total > 0 {
                (dead as f64 / total as f64) * 100.0
            } else {
                0.0
            };

            // Only report if dead ratio exceeds 10%.
            if dead_pct < 10.0 {
                continue;
            }

            let severity = if dead_pct > 30.0 {
                Severity::Critical
            } else {
                Severity::Warning
            };

            findings.push(VacuumFinding {
                kind: VacuumFindingKind::HighDeadTuples,
                description: format!(
                    "{dead} dead tuples ({dead_pct:.1}% of total), \
                     {live} live tuples"
                ),
                severity,
                evidence_class: EvidenceClass::Heuristic,
                suggested_action: Some(format!("VACUUM ANALYZE {schema}.{table}")),
                schema,
                table,
            });
        }
    }
}

async fn collect_xid_age_findings(
    client: &tokio_postgres::Client,
    findings: &mut Vec<VacuumFinding>,
) {
    let Ok(messages) = client.simple_query(XID_AGE_SQL).await else {
        return;
    };
    for msg in messages {
        if let tokio_postgres::SimpleQueryMessage::Row(row) = msg {
            // table_name is an oid::regclass — may include schema prefix.
            let table_name = row.get(0).unwrap_or("").to_owned();
            let xid_age: i64 = row.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
            let freeze_max: i64 = row.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);

            if freeze_max == 0 {
                continue;
            }

            // XID age fits in 2^31; precision loss is acceptable for a ratio.
            #[allow(clippy::cast_precision_loss)]
            let pct_of_max = (xid_age as f64 / freeze_max as f64) * 100.0;

            // Only report if age exceeds 50% of freeze_max_age.
            if pct_of_max < 50.0 {
                continue;
            }

            let severity = if pct_of_max >= 80.0 {
                Severity::Critical
            } else {
                Severity::Warning
            };

            // table_name from regclass may be "schema.table" or just "table".
            let (schema, table) = split_regclass(&table_name);

            findings.push(VacuumFinding {
                kind: VacuumFindingKind::XidWraparoundRisk,
                description: format!(
                    "XID age {xid_age} is {pct_of_max:.1}% of \
                     autovacuum_freeze_max_age ({freeze_max})"
                ),
                severity,
                evidence_class: EvidenceClass::Heuristic,
                suggested_action: Some(format!(
                    "VACUUM FREEZE {schema}.{table} \
                     -- or lower vacuum_freeze_min_age on this table"
                )),
                schema,
                table,
            });
        }
    }
}

/// Collect stale-table findings by re-using the dead tuple query result.
///
/// Runs the dead-tuple query again (filtering for tables with no vacuum in
/// > 7 days). This deliberately keeps each check independent and readable.
async fn collect_stale_table_findings(
    client: &tokio_postgres::Client,
    findings: &mut Vec<VacuumFinding>,
) {
    // Re-query pg_stat_user_tables for staleness; the dead-tuple query
    // already has `last_vacuum` / `last_autovacuum`, but we want all tables
    // (not just those with > 1,000 dead tuples).
    let sql = "\
        select \
            schemaname, \
            relname, \
            last_vacuum, \
            last_autovacuum \
        from pg_stat_user_tables \
        where \
            (last_vacuum is null or now() - last_vacuum > interval '7 days') \
            and (last_autovacuum is null or now() - last_autovacuum > interval '7 days') \
            and n_live_tup > 1000 \
        order by relname \
        limit 20";

    let Ok(messages) = client.simple_query(sql).await else {
        return;
    };
    for msg in messages {
        if let tokio_postgres::SimpleQueryMessage::Row(row) = msg {
            let schema = row.get(0).unwrap_or("").to_owned();
            let table = row.get(1).unwrap_or("").to_owned();
            let last_vacuum = row.get(2).map(ToOwned::to_owned);
            let last_autovacuum = row.get(3).map(ToOwned::to_owned);

            let last_any = last_vacuum
                .as_deref()
                .or(last_autovacuum.as_deref())
                .unwrap_or("never");

            findings.push(VacuumFinding {
                kind: VacuumFindingKind::StaleTable,
                description: format!(
                    "Table has not been vacuumed in > 7 days \
                     (last vacuum: {last_any})"
                ),
                severity: Severity::Warning,
                evidence_class: EvidenceClass::Heuristic,
                suggested_action: Some(format!("VACUUM ANALYZE {schema}.{table}")),
                schema,
                table,
            });
        }
    }
}

async fn collect_autovacuum_worker_count(
    client: &tokio_postgres::Client,
    findings: &mut Vec<VacuumFinding>,
) {
    let Ok(messages) = client.simple_query(AUTOVACUUM_WORKERS_SQL).await else {
        return;
    };
    let mut worker_count: i64 = 0;
    for msg in messages {
        if let tokio_postgres::SimpleQueryMessage::Row(row) = msg {
            worker_count = row.get(0).and_then(|s| s.parse().ok()).unwrap_or(0);
        }
    }

    findings.push(VacuumFinding {
        kind: VacuumFindingKind::AutovacuumWorkerCount,
        schema: String::new(),
        table: String::new(),
        description: format!("{worker_count} autovacuum worker(s) currently running"),
        severity: Severity::Info,
        evidence_class: EvidenceClass::Factual,
        suggested_action: None,
    });
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Split a `regclass` result (e.g. `"public.orders"` or `"orders"`) into
/// `(schema, table)`. Returns `("public", table)` when no schema prefix is
/// present.
fn split_regclass(name: &str) -> (String, String) {
    if let Some((schema, table)) = name.split_once('.') {
        (schema.to_owned(), table.to_owned())
    } else {
        ("public".to_owned(), name.to_owned())
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finding_kind_labels() {
        assert_eq!(
            VacuumFindingKind::HighDeadTuples.label(),
            "high_dead_tuples"
        );
        assert_eq!(
            VacuumFindingKind::XidWraparoundRisk.label(),
            "xid_wraparound_risk"
        );
        assert_eq!(VacuumFindingKind::StaleTable.label(), "stale_table");
        assert_eq!(
            VacuumFindingKind::AutovacuumWorkerCount.label(),
            "autovacuum_workers"
        );
    }

    #[test]
    fn finding_kind_evidence_classes() {
        assert_eq!(
            VacuumFindingKind::AutovacuumWorkerCount.evidence_class(),
            EvidenceClass::Factual
        );
        assert_eq!(
            VacuumFindingKind::HighDeadTuples.evidence_class(),
            EvidenceClass::Heuristic
        );
        assert_eq!(
            VacuumFindingKind::XidWraparoundRisk.evidence_class(),
            EvidenceClass::Heuristic
        );
        assert_eq!(
            VacuumFindingKind::StaleTable.evidence_class(),
            EvidenceClass::Heuristic
        );
    }

    #[test]
    fn empty_report_display_message() {
        let report = VacuumReport {
            findings: Vec::new(),
        };
        assert!(report.to_prompt().contains("No vacuum health issues"));
    }

    #[test]
    fn report_to_prompt_with_findings() {
        let report = VacuumReport {
            findings: vec![VacuumFinding {
                kind: VacuumFindingKind::HighDeadTuples,
                schema: "public".to_owned(),
                table: "orders".to_owned(),
                description: "50000 dead tuples (25.0% of total), 150000 live tuples".to_owned(),
                severity: Severity::Warning,
                evidence_class: EvidenceClass::Heuristic,
                suggested_action: Some("VACUUM ANALYZE public.orders".to_owned()),
            }],
        };
        let prompt = report.to_prompt();
        assert!(prompt.contains("1 finding"));
        assert!(prompt.contains("[high_dead_tuples]"));
        assert!(prompt.contains("public.orders"));
        assert!(prompt.contains("VACUUM ANALYZE"));
    }

    #[test]
    fn report_sorts_by_severity() {
        let mut report = VacuumReport {
            findings: vec![
                VacuumFinding {
                    kind: VacuumFindingKind::StaleTable,
                    schema: "s".to_owned(),
                    table: "t1".to_owned(),
                    description: "stale".to_owned(),
                    severity: Severity::Warning,
                    evidence_class: EvidenceClass::Heuristic,
                    suggested_action: None,
                },
                VacuumFinding {
                    kind: VacuumFindingKind::XidWraparoundRisk,
                    schema: "s".to_owned(),
                    table: "t2".to_owned(),
                    description: "xid risk".to_owned(),
                    severity: Severity::Critical,
                    evidence_class: EvidenceClass::Heuristic,
                    suggested_action: None,
                },
                VacuumFinding {
                    kind: VacuumFindingKind::AutovacuumWorkerCount,
                    schema: String::new(),
                    table: String::new(),
                    description: "2 workers".to_owned(),
                    severity: Severity::Info,
                    evidence_class: EvidenceClass::Factual,
                    suggested_action: None,
                },
            ],
        };
        report.findings.sort_by(|a, b| b.severity.cmp(&a.severity));
        assert_eq!(report.findings[0].severity, Severity::Critical);
        assert_eq!(report.findings[1].severity, Severity::Warning);
        assert_eq!(report.findings[2].severity, Severity::Info);
    }

    #[test]
    fn split_regclass_with_schema() {
        let (schema, table) = split_regclass("public.orders");
        assert_eq!(schema, "public");
        assert_eq!(table, "orders");
    }

    #[test]
    fn split_regclass_without_schema() {
        let (schema, table) = split_regclass("orders");
        assert_eq!(schema, "public");
        assert_eq!(table, "orders");
    }

    #[test]
    fn split_regclass_custom_schema() {
        let (schema, table) = split_regclass("analytics.events");
        assert_eq!(schema, "analytics");
        assert_eq!(table, "events");
    }

    #[test]
    fn dead_tuples_sql_filters_threshold() {
        assert!(DEAD_TUPLES_SQL.contains("n_dead_tup > 1000"));
        assert!(DEAD_TUPLES_SQL.contains("pg_stat_user_tables"));
    }

    #[test]
    fn xid_age_sql_excludes_system_schemas() {
        assert!(XID_AGE_SQL.contains("pg_catalog"));
        assert!(XID_AGE_SQL.contains("information_schema"));
        assert!(XID_AGE_SQL.contains("relfrozenxid"));
    }

    #[test]
    fn autovacuum_workers_sql_correct_backend_type() {
        assert!(AUTOVACUUM_WORKERS_SQL.contains("autovacuum worker"));
        assert!(AUTOVACUUM_WORKERS_SQL.contains("pg_stat_activity"));
    }

    #[test]
    fn high_dead_pct_gives_critical_severity() {
        // Simulate the severity logic: > 30% dead → Critical.
        let dead_pct = 35.0_f64;
        let severity = if dead_pct > 30.0 {
            Severity::Critical
        } else {
            Severity::Warning
        };
        assert_eq!(severity, Severity::Critical);
    }

    #[test]
    fn moderate_dead_pct_gives_warning_severity() {
        let dead_pct = 15.0_f64;
        let severity = if dead_pct > 30.0 {
            Severity::Critical
        } else {
            Severity::Warning
        };
        assert_eq!(severity, Severity::Warning);
    }

    #[test]
    #[allow(clippy::cast_precision_loss)]
    fn xid_age_critical_at_80_pct() {
        let xid_age = 160_000_000_i64;
        let freeze_max = 200_000_000_i64;
        let pct = (xid_age as f64 / freeze_max as f64) * 100.0;
        let severity = if pct >= 80.0 {
            Severity::Critical
        } else {
            Severity::Warning
        };
        assert_eq!(severity, Severity::Critical);
    }

    #[test]
    #[allow(clippy::cast_precision_loss)]
    fn xid_age_warning_at_60_pct() {
        let xid_age = 120_000_000_i64;
        let freeze_max = 200_000_000_i64;
        let pct = (xid_age as f64 / freeze_max as f64) * 100.0;
        let severity = if pct >= 80.0 {
            Severity::Critical
        } else {
            Severity::Warning
        };
        assert_eq!(severity, Severity::Warning);
    }
}
