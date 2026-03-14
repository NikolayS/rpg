//! Bloat Analyzer — detects table and index bloat, oversized unused indexes,
//! and indexes larger than their parent tables.
//!
//! Operates at Observe level: reads `pg_catalog` and `pg_stat_*` views
//! to produce structured findings. No writes are performed.
//!
//! # Sub-findings (from SPEC)
//!
//! | Sub-finding | Evidence Class | Source |
//! |---|---|---|
//! | High table bloat | Heuristic | Size vs live tuples via `pg_stat_user_tables` |
//! | Index larger than table | Heuristic | `pg_relation_size` comparison |
//! | Large unused index | Heuristic | `idx_scan` = 0 on large indexes |
//!
//! When `pgstattuple` is available the analyzer records it in
//! [`BloatReport::pgstattuple_available`] for callers that want to
//! offer a more precise follow-up query.

// Phase 2 infrastructure — compiled but not yet wired into the main dispatch loop.
#![allow(dead_code)]

use crate::governance::{EvidenceClass, Severity};

use std::fmt::Write as _;

// ---------------------------------------------------------------------------
// Finding types
// ---------------------------------------------------------------------------

/// Category of bloat finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BloatFindingKind {
    /// Table estimated bloat exceeds the warning threshold (> 30%).
    TableBloat,
    /// Index size exceeds its parent table size.
    IndexLargerThanTable,
    /// Index has never been scanned and is large (potential dead weight).
    LargeUnusedIndex,
}

impl BloatFindingKind {
    /// Evidence class for this finding kind.
    ///
    /// All bloat findings are heuristic — they use size-based estimates
    /// that may have false positives without `pgstattuple`.
    pub fn evidence_class() -> EvidenceClass {
        EvidenceClass::Heuristic
    }

    /// Human-readable label.
    pub fn label(self) -> &'static str {
        match self {
            Self::TableBloat => "table_bloat",
            Self::IndexLargerThanTable => "index_larger_than_table",
            Self::LargeUnusedIndex => "large_unused_index",
        }
    }
}

/// A single bloat finding.
#[derive(Debug, Clone)]
pub struct BloatFinding {
    /// What kind of finding.
    pub kind: BloatFindingKind,
    /// Qualified table name (`schema.table`).
    pub table_name: String,
    /// Qualified index name (`schema.index`) — empty for table-only findings.
    pub index_name: String,
    /// Human-readable description of the problem.
    pub description: String,
    /// Severity level.
    pub severity: Severity,
    /// Evidence class (always [`EvidenceClass::Heuristic`] for this analyzer).
    pub evidence_class: EvidenceClass,
    /// Suggested remediation SQL (Observe mode: informational only).
    pub suggested_action: Option<String>,
    /// Size of the object in bytes (table total or index).
    pub size_bytes: Option<i64>,
}

/// Complete bloat analysis report.
#[derive(Debug, Clone)]
pub struct BloatReport {
    /// All findings, sorted by severity (critical first).
    pub findings: Vec<BloatFinding>,
    /// Whether the `pgstattuple` extension is available in this database.
    /// When `true`, callers can run precise bloat queries using it.
    pub pgstattuple_available: bool,
}

impl BloatReport {
    /// Display the report to stderr.
    pub fn display(&self) {
        if self.findings.is_empty() {
            eprintln!("Bloat analysis: no issues found.");
        } else {
            eprintln!(
                "Bloat analysis: {} issue{} found.\n",
                self.findings.len(),
                if self.findings.len() == 1 { "" } else { "s" }
            );
            for f in &self.findings {
                let icon = match f.severity {
                    Severity::Critical => "!!",
                    Severity::Warning => "! ",
                    Severity::Info => "  ",
                };
                eprintln!(
                    "{icon} [{kind}] {table}",
                    kind = f.kind.label(),
                    table = f.table_name,
                );
                if !f.index_name.is_empty() {
                    eprintln!("   index: {}", f.index_name);
                }
                eprintln!("   {}", f.description);
                if let Some(ref action) = f.suggested_action {
                    eprintln!("   suggestion: {action}");
                }
                if let Some(bytes) = f.size_bytes {
                    eprintln!("   size: {}", format_bytes(bytes));
                }
                eprintln!();
            }
        }

        if self.pgstattuple_available {
            eprintln!(
                "Note: pgstattuple is available — \
                 use it for precise per-table bloat measurement."
            );
        }
    }

    /// Build a text summary suitable for LLM consumption.
    pub fn to_prompt(&self) -> String {
        if self.findings.is_empty() {
            return "No bloat issues found.".to_owned();
        }
        let mut out = format!(
            "Bloat analysis report: {} finding(s)\n\n",
            self.findings.len()
        );
        for (i, f) in self.findings.iter().enumerate() {
            let _ = write!(
                out,
                "{}. [{kind}] {table}",
                i + 1,
                kind = f.kind.label(),
                table = f.table_name,
            );
            if !f.index_name.is_empty() {
                let _ = write!(out, " (index: {})", f.index_name);
            }
            out.push('\n');
            let _ = writeln!(out, "   {}", f.description);
            if let Some(ref action) = f.suggested_action {
                let _ = writeln!(out, "   Suggested: {action}");
            }
            out.push('\n');
        }
        if self.pgstattuple_available {
            out.push_str("Note: pgstattuple is available for precise bloat measurement.\n");
        }
        out
    }

    /// Convert actionable findings into [`crate::governance::ActionProposal`]s
    /// for Supervised mode.
    pub fn to_proposals(&self) -> Vec<crate::governance::ActionProposal> {
        self.findings
            .iter()
            .filter_map(BloatFinding::to_proposal)
            .collect()
    }
}

impl BloatFinding {
    /// Convert this finding into an [`crate::governance::ActionProposal`].
    ///
    /// Returns `None` for findings without a concrete suggested action.
    pub fn to_proposal(&self) -> Option<crate::governance::ActionProposal> {
        let action = self.suggested_action.as_ref()?;

        let risk = match self.kind {
            BloatFindingKind::TableBloat => {
                "VACUUM FULL requires an exclusive lock and rewrites the table in-place. \
                 pg_repack performs the same operation online — prefer it for production. \
                 Schedule during low-traffic windows."
            }
            BloatFindingKind::IndexLargerThanTable => {
                "REINDEX CONCURRENTLY rebuilds the index without blocking writes. \
                 May cause temporary increased I/O during rebuild."
            }
            BloatFindingKind::LargeUnusedIndex => {
                "Dropping an unused index frees disk space and reduces write overhead. \
                 Verify the index has been unused for a sufficient period after a stats reset."
            }
        };

        let expected = match self.kind {
            BloatFindingKind::TableBloat => {
                let size = self.size_bytes.map_or_else(String::new, |b| {
                    format!(", reclaiming ~{}", format_bytes(b))
                });
                format!("Reduce bloat on {}{size}", self.table_name)
            }
            BloatFindingKind::IndexLargerThanTable => format!(
                "Rebuild oversized index {} to compact storage",
                self.index_name
            ),
            BloatFindingKind::LargeUnusedIndex => {
                let size = self
                    .size_bytes
                    .map_or_else(String::new, |b| format!(", freeing {}", format_bytes(b)));
                format!("Drop unused index {}{size}", self.index_name)
            }
        };

        Some(crate::governance::ActionProposal {
            feature: crate::governance::FeatureArea::Bloat,
            severity: self.severity,
            evidence_class: self.evidence_class,
            finding: self.description.clone(),
            proposed_action: action.clone(),
            expected_outcome: expected,
            risk: risk.to_owned(),
            created_at: std::time::SystemTime::now(),
        })
    }
}

// ---------------------------------------------------------------------------
// SQL queries
// ---------------------------------------------------------------------------

/// Estimate table bloat via live tuple count vs actual relation size.
///
/// Only considers tables larger than 1 MiB to avoid noise.
/// Returns the top 20 tables by total size (table + indexes + toast).
const TABLE_BLOAT_SQL: &str = "\
    select \
        schemaname || '.' || tablename as table_name, \
        pg_total_relation_size(schemaname || '.' || tablename) as total_bytes, \
        pg_relation_size(schemaname || '.' || tablename) as table_bytes, \
        case when pg_relation_size(schemaname || '.' || tablename) > 0 \
            then round(100.0 * (1.0 - (n_live_tup * 8192.0) / \
                nullif(pg_relation_size(schemaname || '.' || tablename), 0)), 1) \
            else 0 end as estimated_bloat_pct \
    from pg_stat_user_tables \
    where pg_relation_size(schemaname || '.' || tablename) > 1048576 \
    order by pg_total_relation_size(schemaname || '.' || tablename) desc \
    limit 20";

/// Detect indexes that are larger than their parent table, or have 0 scans
/// and are large enough to be worth investigating.
///
/// Only considers indexes larger than 1 MiB.
const INDEX_BLOAT_SQL: &str = "\
    select \
        schemaname || '.' || indexrelname as index_name, \
        schemaname || '.' || relname as table_name, \
        pg_relation_size(indexrelid) as index_bytes, \
        pg_relation_size(relid) as table_bytes, \
        idx_scan as index_scans \
    from pg_stat_user_indexes \
    where pg_relation_size(indexrelid) > 1048576 \
    order by pg_relation_size(indexrelid) desc \
    limit 20";

/// Check whether pgstattuple is installed in the current database.
const PGSTATTUPLE_CHECK_SQL: &str = "select 1 from pg_extension where extname = 'pgstattuple'";

// ---------------------------------------------------------------------------
// Bloat thresholds
// ---------------------------------------------------------------------------

/// Tables with estimated bloat above this percentage trigger a finding.
const TABLE_BLOAT_WARN_PCT: f64 = 30.0;
const TABLE_BLOAT_CRITICAL_PCT: f64 = 70.0;

// ---------------------------------------------------------------------------
// Analyzer
// ---------------------------------------------------------------------------

/// Bloat analyzer — Observe mode, zero writes.
pub struct BloatAnalyzer;

impl BloatAnalyzer {
    /// Run all bloat checks and return a [`BloatReport`].
    ///
    /// All queries are read-only. The method never panics; individual query
    /// failures are silently skipped so a single unavailable view does not
    /// abort the whole analysis.
    pub async fn analyze(client: &tokio_postgres::Client) -> BloatReport {
        let mut findings = Vec::new();

        // 1. Table bloat.
        collect_table_bloat(client, &mut findings).await;

        // 2. Index bloat (larger than table, large unused).
        collect_index_bloat(client, &mut findings).await;

        // 3. Check pgstattuple availability.
        let pgstattuple_available = check_pgstattuple(client).await;

        // Sort: Critical first, then Warning, then Info.
        findings.sort_by(|a, b| b.severity.cmp(&a.severity));

        BloatReport {
            findings,
            pgstattuple_available,
        }
    }
}

// ---------------------------------------------------------------------------
// Data collection helpers
// ---------------------------------------------------------------------------

async fn collect_table_bloat(client: &tokio_postgres::Client, findings: &mut Vec<BloatFinding>) {
    let Ok(messages) = client.simple_query(TABLE_BLOAT_SQL).await else {
        return;
    };
    for msg in messages {
        let tokio_postgres::SimpleQueryMessage::Row(row) = msg else {
            continue;
        };
        let table_name = row.get(0).unwrap_or("").to_owned();
        let total_bytes: i64 = row.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
        let bloat_pct: f64 = row.get(3).and_then(|s| s.parse().ok()).unwrap_or(0.0);

        if bloat_pct <= TABLE_BLOAT_WARN_PCT {
            continue;
        }

        let severity = if bloat_pct >= TABLE_BLOAT_CRITICAL_PCT {
            Severity::Critical
        } else {
            Severity::Warning
        };

        // Suggest pg_repack for large tables; VACUUM FULL as a fallback.
        let suggested_action = if total_bytes >= 1_073_741_824 {
            // >= 1 GiB — strongly prefer online tool.
            Some(format!(
                "pg_repack --table {table_name}  \
                 -- or: VACUUM FULL {table_name}"
            ))
        } else {
            Some(format!("VACUUM FULL {table_name}"))
        };

        findings.push(BloatFinding {
            kind: BloatFindingKind::TableBloat,
            table_name: table_name.clone(),
            index_name: String::new(),
            description: format!(
                "Table {table_name} is estimated ~{bloat_pct}% bloated \
                 (total size: {sz})",
                sz = format_bytes(total_bytes),
            ),
            severity,
            evidence_class: EvidenceClass::Heuristic,
            suggested_action,
            size_bytes: Some(total_bytes),
        });
    }
}

async fn collect_index_bloat(client: &tokio_postgres::Client, findings: &mut Vec<BloatFinding>) {
    let Ok(messages) = client.simple_query(INDEX_BLOAT_SQL).await else {
        return;
    };
    for msg in messages {
        let tokio_postgres::SimpleQueryMessage::Row(row) = msg else {
            continue;
        };
        let index_name = row.get(0).unwrap_or("").to_owned();
        let table_name = row.get(1).unwrap_or("").to_owned();
        let index_bytes: i64 = row.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);
        let table_bytes: i64 = row.get(3).and_then(|s| s.parse().ok()).unwrap_or(0);
        let index_scans: i64 = row.get(4).and_then(|s| s.parse().ok()).unwrap_or(0);

        // Finding 1: index larger than its table.
        if index_bytes > table_bytes && table_bytes > 0 {
            findings.push(BloatFinding {
                kind: BloatFindingKind::IndexLargerThanTable,
                table_name: table_name.clone(),
                index_name: index_name.clone(),
                description: format!(
                    "Index {index_name} ({idx_sz}) is larger than its \
                     parent table {table_name} ({tbl_sz}) — likely bloated",
                    idx_sz = format_bytes(index_bytes),
                    tbl_sz = format_bytes(table_bytes),
                ),
                severity: Severity::Warning,
                evidence_class: EvidenceClass::Heuristic,
                suggested_action: Some(format!("REINDEX INDEX CONCURRENTLY {index_name}")),
                size_bytes: Some(index_bytes),
            });
        }

        // Finding 2: large index with 0 scans (never used).
        if index_scans == 0 {
            findings.push(BloatFinding {
                kind: BloatFindingKind::LargeUnusedIndex,
                table_name: table_name.clone(),
                index_name: index_name.clone(),
                description: format!(
                    "Index {index_name} ({sz}) has never been scanned — \
                     consider dropping it",
                    sz = format_bytes(index_bytes),
                ),
                severity: Severity::Warning,
                evidence_class: EvidenceClass::Heuristic,
                suggested_action: Some(format!("DROP INDEX CONCURRENTLY {index_name}")),
                size_bytes: Some(index_bytes),
            });
        }
    }
}

async fn check_pgstattuple(client: &tokio_postgres::Client) -> bool {
    let Ok(messages) = client.simple_query(PGSTATTUPLE_CHECK_SQL).await else {
        return false;
    };
    messages
        .iter()
        .any(|m| matches!(m, tokio_postgres::SimpleQueryMessage::Row(_)))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Format bytes into a human-readable string using binary units (GiB/MiB/KiB).
#[allow(clippy::cast_precision_loss)]
fn format_bytes(bytes: i64) -> String {
    const KIB: i64 = 1024;
    const MIB: i64 = 1024 * KIB;
    const GIB: i64 = 1024 * MIB;

    if bytes >= GIB {
        format!("{:.1} GiB", bytes as f64 / GIB as f64)
    } else if bytes >= MIB {
        format!("{:.1} MiB", bytes as f64 / MIB as f64)
    } else if bytes >= KIB {
        format!("{:.1} KiB", bytes as f64 / KIB as f64)
    } else {
        format!("{bytes} B")
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
        assert_eq!(BloatFindingKind::TableBloat.label(), "table_bloat");
        assert_eq!(
            BloatFindingKind::IndexLargerThanTable.label(),
            "index_larger_than_table"
        );
        assert_eq!(
            BloatFindingKind::LargeUnusedIndex.label(),
            "large_unused_index"
        );
    }

    #[test]
    fn finding_kind_evidence_class() {
        assert_eq!(BloatFindingKind::evidence_class(), EvidenceClass::Heuristic);
    }

    #[test]
    fn format_bytes_display() {
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(2048), "2.0 KiB");
        assert_eq!(format_bytes(10_485_760), "10.0 MiB");
        assert_eq!(format_bytes(1_073_741_824), "1.0 GiB");
    }

    #[test]
    fn empty_report_display_and_prompt() {
        let report = BloatReport {
            findings: Vec::new(),
            pgstattuple_available: false,
        };
        assert!(report.to_prompt().contains("No bloat issues found."));
    }

    #[test]
    fn report_to_prompt_with_findings() {
        let report = BloatReport {
            findings: vec![BloatFinding {
                kind: BloatFindingKind::TableBloat,
                table_name: "public.orders".to_owned(),
                index_name: String::new(),
                description: "Table is ~55% bloated".to_owned(),
                severity: Severity::Warning,
                evidence_class: EvidenceClass::Heuristic,
                suggested_action: Some("VACUUM FULL public.orders".to_owned()),
                size_bytes: Some(52_428_800),
            }],
            pgstattuple_available: false,
        };
        let prompt = report.to_prompt();
        assert!(prompt.contains("1 finding"));
        assert!(prompt.contains("[table_bloat]"));
        assert!(prompt.contains("public.orders"));
        assert!(prompt.contains("VACUUM FULL"));
    }

    #[test]
    fn report_to_prompt_pgstattuple_note() {
        let report = BloatReport {
            findings: Vec::new(),
            pgstattuple_available: true,
        };
        let prompt = report.to_prompt();
        // Empty findings return early without pgstattuple note in the prompt.
        assert!(prompt.contains("No bloat issues found."));

        let report_with_findings = BloatReport {
            findings: vec![BloatFinding {
                kind: BloatFindingKind::LargeUnusedIndex,
                table_name: "public.t".to_owned(),
                index_name: "public.idx_old".to_owned(),
                description: "Index never scanned".to_owned(),
                severity: Severity::Warning,
                evidence_class: EvidenceClass::Heuristic,
                suggested_action: Some("DROP INDEX CONCURRENTLY public.idx_old".to_owned()),
                size_bytes: Some(5_242_880),
            }],
            pgstattuple_available: true,
        };
        let prompt2 = report_with_findings.to_prompt();
        assert!(prompt2.contains("pgstattuple"));
    }

    #[test]
    fn report_sorts_by_severity() {
        let mut report = BloatReport {
            findings: vec![
                BloatFinding {
                    kind: BloatFindingKind::LargeUnusedIndex,
                    table_name: "public.t".to_owned(),
                    index_name: "public.idx_a".to_owned(),
                    description: "unused".to_owned(),
                    severity: Severity::Warning,
                    evidence_class: EvidenceClass::Heuristic,
                    suggested_action: None,
                    size_bytes: None,
                },
                BloatFinding {
                    kind: BloatFindingKind::TableBloat,
                    table_name: "public.big".to_owned(),
                    index_name: String::new(),
                    description: "very bloated".to_owned(),
                    severity: Severity::Critical,
                    evidence_class: EvidenceClass::Heuristic,
                    suggested_action: None,
                    size_bytes: None,
                },
            ],
            pgstattuple_available: false,
        };
        report.findings.sort_by(|a, b| b.severity.cmp(&a.severity));
        assert_eq!(report.findings[0].severity, Severity::Critical);
        assert_eq!(report.findings[1].severity, Severity::Warning);
    }

    #[test]
    fn table_bloat_sql_has_threshold() {
        // 1 MiB minimum filter.
        assert!(TABLE_BLOAT_SQL.contains("1048576"));
        assert!(TABLE_BLOAT_SQL.contains("pg_stat_user_tables"));
        assert!(TABLE_BLOAT_SQL.contains("n_live_tup"));
    }

    #[test]
    fn index_bloat_sql_has_threshold() {
        // 1 MiB minimum filter.
        assert!(INDEX_BLOAT_SQL.contains("1048576"));
        assert!(INDEX_BLOAT_SQL.contains("pg_stat_user_indexes"));
        assert!(INDEX_BLOAT_SQL.contains("idx_scan"));
    }

    #[test]
    fn pgstattuple_sql_checks_extension() {
        assert!(PGSTATTUPLE_CHECK_SQL.contains("pg_extension"));
        assert!(PGSTATTUPLE_CHECK_SQL.contains("pgstattuple"));
    }

    #[test]
    fn finding_to_proposal_table_bloat() {
        let finding = BloatFinding {
            kind: BloatFindingKind::TableBloat,
            table_name: "public.orders".to_owned(),
            index_name: String::new(),
            description: "Estimated 55% bloated".to_owned(),
            severity: Severity::Warning,
            evidence_class: EvidenceClass::Heuristic,
            suggested_action: Some("VACUUM FULL public.orders".to_owned()),
            size_bytes: Some(52_428_800),
        };
        let proposal = finding.to_proposal().unwrap();
        assert_eq!(proposal.feature, crate::governance::FeatureArea::Bloat);
        assert!(proposal.proposed_action.contains("VACUUM FULL"));
        assert!(proposal.risk.contains("exclusive lock"));
    }

    #[test]
    fn finding_to_proposal_large_unused_index() {
        let finding = BloatFinding {
            kind: BloatFindingKind::LargeUnusedIndex,
            table_name: "public.t".to_owned(),
            index_name: "public.idx_old".to_owned(),
            description: "Never scanned".to_owned(),
            severity: Severity::Warning,
            evidence_class: EvidenceClass::Heuristic,
            suggested_action: Some("DROP INDEX CONCURRENTLY public.idx_old".to_owned()),
            size_bytes: Some(10_485_760),
        };
        let proposal = finding.to_proposal().unwrap();
        assert!(proposal.proposed_action.contains("DROP INDEX"));
        assert!(proposal.expected_outcome.contains("public.idx_old"));
    }

    #[test]
    fn finding_to_proposal_index_larger_than_table() {
        let finding = BloatFinding {
            kind: BloatFindingKind::IndexLargerThanTable,
            table_name: "public.events".to_owned(),
            index_name: "public.idx_events_ts".to_owned(),
            description: "Index larger than table".to_owned(),
            severity: Severity::Warning,
            evidence_class: EvidenceClass::Heuristic,
            suggested_action: Some("REINDEX INDEX CONCURRENTLY public.idx_events_ts".to_owned()),
            size_bytes: Some(20_971_520),
        };
        let proposal = finding.to_proposal().unwrap();
        assert!(proposal
            .proposed_action
            .contains("REINDEX INDEX CONCURRENTLY"));
        assert!(proposal.risk.contains("REINDEX CONCURRENTLY"));
    }

    #[test]
    fn finding_to_proposal_no_action_returns_none() {
        let finding = BloatFinding {
            kind: BloatFindingKind::TableBloat,
            table_name: "public.t".to_owned(),
            index_name: String::new(),
            description: "Bloated".to_owned(),
            severity: Severity::Warning,
            evidence_class: EvidenceClass::Heuristic,
            suggested_action: None,
            size_bytes: None,
        };
        assert!(finding.to_proposal().is_none());
    }

    #[test]
    fn report_to_proposals_counts_actionable() {
        let report = BloatReport {
            findings: vec![
                BloatFinding {
                    kind: BloatFindingKind::TableBloat,
                    table_name: "public.a".to_owned(),
                    index_name: String::new(),
                    description: "bloated".to_owned(),
                    severity: Severity::Warning,
                    evidence_class: EvidenceClass::Heuristic,
                    suggested_action: Some("VACUUM FULL public.a".to_owned()),
                    size_bytes: None,
                },
                BloatFinding {
                    kind: BloatFindingKind::TableBloat,
                    table_name: "public.b".to_owned(),
                    index_name: String::new(),
                    description: "bloated no action".to_owned(),
                    severity: Severity::Warning,
                    evidence_class: EvidenceClass::Heuristic,
                    suggested_action: None,
                    size_bytes: None,
                },
            ],
            pgstattuple_available: false,
        };
        let proposals = report.to_proposals();
        assert_eq!(proposals.len(), 1);
    }

    #[test]
    fn large_table_suggests_pg_repack() {
        // Simulate a table >= 1 GiB — should suggest pg_repack first.
        let table_name = "public.huge";
        let total_bytes: i64 = 2 * 1_073_741_824; // 2 GiB
        let action = if total_bytes >= 1_073_741_824 {
            format!("pg_repack --table {table_name}  -- or: VACUUM FULL {table_name}")
        } else {
            format!("VACUUM FULL {table_name}")
        };
        assert!(action.contains("pg_repack"));
        assert!(action.contains("VACUUM FULL"));
    }
}
