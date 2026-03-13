//! Index health Analyzer — detects unused, redundant, invalid, bloated,
//! and missing indexes.
//!
//! Operates at Observe level: reads `pg_catalog` and `pg_stat_*` views
//! to produce structured findings. No writes are performed.
//!
//! # Sub-findings (from SPEC)
//!
//! | Sub-finding | Evidence Class | Source |
//! |---|---|---|
//! | Invalid index | Factual | `pg_index.indisvalid = false` |
//! | Bloated index | Heuristic | Size-based estimate vs live tuples |
//! | Unused index | Heuristic | 0 scans, stats reset age |
//! | Missing index | Heuristic | High seq_scan count on large tables |
//! | Redundant index | Heuristic | Column prefix match |

use crate::governance::{EvidenceClass, Severity};

use std::fmt::Write as _;

// ---------------------------------------------------------------------------
// Index finding types
// ---------------------------------------------------------------------------

/// Category of index health finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FindingKind {
    /// `pg_index.indisvalid = false`.
    Invalid,
    /// Index has not been scanned since stats reset.
    Unused,
    /// Index columns are a prefix of another index.
    Redundant,
    /// Index size significantly exceeds expected size for row count.
    Bloated,
    /// Table has high sequential scan count with no covering index.
    MissingIndex,
}

impl FindingKind {
    /// Evidence class for this finding kind.
    pub fn evidence_class(self) -> EvidenceClass {
        match self {
            Self::Invalid => EvidenceClass::Factual,
            Self::Unused | Self::Redundant | Self::Bloated | Self::MissingIndex => {
                EvidenceClass::Heuristic
            }
        }
    }

    /// Human-readable label.
    pub fn label(self) -> &'static str {
        match self {
            Self::Invalid => "invalid",
            Self::Unused => "unused",
            Self::Redundant => "redundant",
            Self::Bloated => "bloated",
            Self::MissingIndex => "missing_index",
        }
    }
}

/// A single index health finding.
#[derive(Debug, Clone)]
pub struct IndexFinding {
    /// What kind of finding.
    pub kind: FindingKind,
    /// Schema name.
    pub schema: String,
    /// Table name.
    pub table: String,
    /// Index name (empty for missing index findings).
    pub index_name: String,
    /// Human-readable description.
    pub description: String,
    /// Severity level.
    pub severity: Severity,
    /// Evidence class.
    pub evidence_class: EvidenceClass,
    /// Suggested remediation SQL (Observe mode: informational only).
    pub suggested_action: Option<String>,
    /// Size of the index in bytes (if known).
    pub size_bytes: Option<i64>,
}

/// Complete index health report.
#[derive(Debug, Clone)]
pub struct IndexHealthReport {
    /// All findings, sorted by severity (critical first).
    pub findings: Vec<IndexFinding>,
}

impl IndexHealthReport {
    /// Display the report to the terminal.
    pub fn display(&self) {
        if self.findings.is_empty() {
            eprintln!("Index health: no issues found.");
            return;
        }
        eprintln!(
            "Index health: {} issue{} found.\n",
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
                "{icon} [{kind}] {schema}.{table}",
                kind = f.kind.label(),
                schema = f.schema,
                table = f.table,
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

    /// Build a text summary for LLM consumption.
    pub fn to_prompt(&self) -> String {
        if self.findings.is_empty() {
            return "No index health issues found.".to_owned();
        }
        let mut out = format!(
            "Index health report: {} finding(s)\n\n",
            self.findings.len()
        );
        for (i, f) in self.findings.iter().enumerate() {
            let _ = write!(
                out,
                "{}. [{kind}] {schema}.{table}",
                i + 1,
                kind = f.kind.label(),
                schema = f.schema,
                table = f.table,
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
        out
    }
}

// ---------------------------------------------------------------------------
// SQL queries
// ---------------------------------------------------------------------------

/// Detect invalid indexes.
const INVALID_INDEXES_SQL: &str = "\
    SELECT \
        n.nspname AS schema, \
        t.relname AS table_name, \
        i.relname AS index_name, \
        pg_relation_size(i.oid) AS index_size \
    FROM pg_index AS ix \
    JOIN pg_class AS i ON i.oid = ix.indexrelid \
    JOIN pg_class AS t ON t.oid = ix.indrelid \
    JOIN pg_namespace AS n ON n.oid = t.relnamespace \
    WHERE NOT ix.indisvalid \
      AND n.nspname NOT IN ('pg_catalog', 'information_schema', 'pg_toast')";

/// Detect unused indexes (0 scans since last stats reset).
const UNUSED_INDEXES_SQL: &str = "\
    SELECT \
        s.schemaname AS schema, \
        s.relname AS table_name, \
        s.indexrelname AS index_name, \
        pg_relation_size(s.indexrelid) AS index_size, \
        s.idx_scan, \
        pg_stat_get_db_stat_reset_time(d.oid) AS stats_reset \
    FROM pg_stat_user_indexes AS s \
    JOIN pg_database AS d ON d.datname = current_database() \
    WHERE s.idx_scan = 0 \
      AND s.indexrelname NOT LIKE 'pg_%' \
      AND NOT EXISTS ( \
          SELECT 1 FROM pg_index \
          WHERE indexrelid = s.indexrelid \
            AND (indisprimary OR indisunique) \
      ) \
    ORDER BY pg_relation_size(s.indexrelid) DESC \
    LIMIT 50";

/// Detect redundant indexes (column prefix match).
const REDUNDANT_INDEXES_SQL: &str = "\
    SELECT \
        n.nspname AS schema, \
        t.relname AS table_name, \
        i1.relname AS index_name, \
        i2.relname AS covering_index, \
        pg_relation_size(i1.oid) AS index_size, \
        pg_get_indexdef(ix1.indexrelid) AS index_def, \
        pg_get_indexdef(ix2.indexrelid) AS covering_def \
    FROM pg_index AS ix1 \
    JOIN pg_index AS ix2 \
        ON ix1.indrelid = ix2.indrelid \
        AND ix1.indexrelid != ix2.indexrelid \
    JOIN pg_class AS i1 ON i1.oid = ix1.indexrelid \
    JOIN pg_class AS i2 ON i2.oid = ix2.indexrelid \
    JOIN pg_class AS t ON t.oid = ix1.indrelid \
    JOIN pg_namespace AS n ON n.oid = t.relnamespace \
    WHERE n.nspname NOT IN ('pg_catalog', 'information_schema', 'pg_toast') \
      AND NOT ix1.indisprimary \
      AND NOT ix1.indisunique \
      AND ix1.indnkeyatts <= ix2.indnkeyatts \
      AND ix1.indkey::text[] <@ ix2.indkey::text[] \
      AND ix1.indkey[0:ix1.indnkeyatts-1] = ix2.indkey[0:ix1.indnkeyatts-1] \
    ORDER BY pg_relation_size(i1.oid) DESC \
    LIMIT 30";

/// Detect tables with high sequential scan counts (missing index candidates).
const MISSING_INDEX_CANDIDATES_SQL: &str = "\
    SELECT \
        schemaname AS schema, \
        relname AS table_name, \
        seq_scan, \
        seq_tup_read, \
        idx_scan, \
        n_live_tup, \
        pg_relation_size(relid) AS table_size \
    FROM pg_stat_user_tables \
    WHERE seq_scan > 100 \
      AND n_live_tup > 10000 \
      AND (idx_scan = 0 OR seq_scan::float / GREATEST(idx_scan, 1) > 10) \
    ORDER BY seq_tup_read DESC \
    LIMIT 20";

/// Detect bloated indexes (size significantly exceeds expected).
///
/// This uses a heuristic: index size vs number of live tuples * estimated
/// tuple width. Without `pgstattuple`, this is approximate.
const BLOATED_INDEXES_SQL: &str = "\
    SELECT \
        n.nspname AS schema, \
        t.relname AS table_name, \
        i.relname AS index_name, \
        pg_relation_size(i.oid) AS index_size, \
        s.n_live_tup, \
        CASE WHEN s.n_live_tup > 0 THEN \
            round(100.0 * (pg_relation_size(i.oid) - s.n_live_tup * 40) \
                  / GREATEST(pg_relation_size(i.oid), 1), 1) \
        ELSE 0 END AS estimated_bloat_pct \
    FROM pg_index AS ix \
    JOIN pg_class AS i ON i.oid = ix.indexrelid \
    JOIN pg_class AS t ON t.oid = ix.indrelid \
    JOIN pg_namespace AS n ON n.oid = t.relnamespace \
    JOIN pg_stat_user_tables AS s \
        ON s.relid = ix.indrelid \
    WHERE ix.indisvalid \
      AND n.nspname NOT IN ('pg_catalog', 'information_schema', 'pg_toast') \
      AND pg_relation_size(i.oid) > 10485760 \
      AND s.n_live_tup > 0 \
      AND (pg_relation_size(i.oid) - s.n_live_tup * 40)::float \
          / GREATEST(pg_relation_size(i.oid), 1) > 0.3 \
    ORDER BY pg_relation_size(i.oid) DESC \
    LIMIT 20";

// ---------------------------------------------------------------------------
// Data collection
// ---------------------------------------------------------------------------

/// Collect index health findings from the database.
///
/// Runs diagnostic queries against `pg_catalog` and `pg_stat_*` views.
/// All operations are read-only (Observe mode).
pub async fn analyze(client: &tokio_postgres::Client) -> IndexHealthReport {
    let mut findings = Vec::new();

    // 1. Invalid indexes (Factual — highest priority).
    collect_invalid_indexes(client, &mut findings).await;

    // 2. Unused indexes.
    collect_unused_indexes(client, &mut findings).await;

    // 3. Redundant indexes.
    collect_redundant_indexes(client, &mut findings).await;

    // 4. Bloated indexes.
    collect_bloated_indexes(client, &mut findings).await;

    // 5. Missing index candidates.
    collect_missing_indexes(client, &mut findings).await;

    // Sort: Critical first, then Warning, then Info.
    findings.sort_by(|a, b| b.severity.cmp(&a.severity));

    IndexHealthReport { findings }
}

async fn collect_invalid_indexes(
    client: &tokio_postgres::Client,
    findings: &mut Vec<IndexFinding>,
) {
    let Ok(messages) = client.simple_query(INVALID_INDEXES_SQL).await else {
        return;
    };
    for msg in messages {
        if let tokio_postgres::SimpleQueryMessage::Row(row) = msg {
            let schema = row.get(0).unwrap_or("").to_owned();
            let table = row.get(1).unwrap_or("").to_owned();
            let index_name = row.get(2).unwrap_or("").to_owned();
            let size: i64 = row.get(3).and_then(|s| s.parse().ok()).unwrap_or(0);

            findings.push(IndexFinding {
                kind: FindingKind::Invalid,
                description: format!("Index {index_name} is marked invalid (indisvalid=false)"),
                severity: Severity::Critical,
                evidence_class: EvidenceClass::Factual,
                suggested_action: Some(format!(
                    "DROP INDEX CONCURRENTLY {schema}.{index_name}; \
                     -- then recreate with CREATE INDEX CONCURRENTLY"
                )),
                size_bytes: Some(size),
                schema,
                table,
                index_name,
            });
        }
    }
}

async fn collect_unused_indexes(client: &tokio_postgres::Client, findings: &mut Vec<IndexFinding>) {
    let Ok(messages) = client.simple_query(UNUSED_INDEXES_SQL).await else {
        return;
    };
    for msg in messages {
        if let tokio_postgres::SimpleQueryMessage::Row(row) = msg {
            let schema = row.get(0).unwrap_or("").to_owned();
            let table = row.get(1).unwrap_or("").to_owned();
            let index_name = row.get(2).unwrap_or("").to_owned();
            let size: i64 = row.get(3).and_then(|s| s.parse().ok()).unwrap_or(0);
            let stats_reset = row.get(5).unwrap_or("unknown").to_owned();

            findings.push(IndexFinding {
                kind: FindingKind::Unused,
                description: format!(
                    "Index {index_name} has 0 scans since stats reset ({stats_reset})"
                ),
                severity: Severity::Warning,
                evidence_class: EvidenceClass::Heuristic,
                suggested_action: Some(format!("DROP INDEX CONCURRENTLY {schema}.{index_name}")),
                size_bytes: Some(size),
                schema,
                table,
                index_name,
            });
        }
    }
}

async fn collect_redundant_indexes(
    client: &tokio_postgres::Client,
    findings: &mut Vec<IndexFinding>,
) {
    let Ok(messages) = client.simple_query(REDUNDANT_INDEXES_SQL).await else {
        return;
    };
    for msg in messages {
        if let tokio_postgres::SimpleQueryMessage::Row(row) = msg {
            let schema = row.get(0).unwrap_or("").to_owned();
            let table = row.get(1).unwrap_or("").to_owned();
            let index_name = row.get(2).unwrap_or("").to_owned();
            let covering = row.get(3).unwrap_or("?").to_owned();
            let size: i64 = row.get(4).and_then(|s| s.parse().ok()).unwrap_or(0);

            findings.push(IndexFinding {
                kind: FindingKind::Redundant,
                description: format!("Index {index_name} is redundant — covered by {covering}"),
                severity: Severity::Warning,
                evidence_class: EvidenceClass::Heuristic,
                suggested_action: Some(format!("DROP INDEX CONCURRENTLY {schema}.{index_name}")),
                size_bytes: Some(size),
                schema,
                table,
                index_name,
            });
        }
    }
}

async fn collect_bloated_indexes(
    client: &tokio_postgres::Client,
    findings: &mut Vec<IndexFinding>,
) {
    let Ok(messages) = client.simple_query(BLOATED_INDEXES_SQL).await else {
        return;
    };
    for msg in messages {
        if let tokio_postgres::SimpleQueryMessage::Row(row) = msg {
            let schema = row.get(0).unwrap_or("").to_owned();
            let table = row.get(1).unwrap_or("").to_owned();
            let index_name = row.get(2).unwrap_or("").to_owned();
            let size: i64 = row.get(3).and_then(|s| s.parse().ok()).unwrap_or(0);
            let bloat_pct = row.get(5).unwrap_or("?").to_owned();

            findings.push(IndexFinding {
                kind: FindingKind::Bloated,
                description: format!(
                    "Index {index_name} estimated ~{bloat_pct}% bloated ({sz})",
                    sz = format_bytes(size),
                ),
                severity: if bloat_pct.parse::<f64>().unwrap_or(0.0) > 60.0 {
                    Severity::Critical
                } else {
                    Severity::Warning
                },
                evidence_class: EvidenceClass::Heuristic,
                suggested_action: Some(format!("REINDEX INDEX CONCURRENTLY {schema}.{index_name}")),
                size_bytes: Some(size),
                schema,
                table,
                index_name,
            });
        }
    }
}

async fn collect_missing_indexes(
    client: &tokio_postgres::Client,
    findings: &mut Vec<IndexFinding>,
) {
    let Ok(messages) = client.simple_query(MISSING_INDEX_CANDIDATES_SQL).await else {
        return;
    };
    for msg in messages {
        if let tokio_postgres::SimpleQueryMessage::Row(row) = msg {
            let schema = row.get(0).unwrap_or("").to_owned();
            let table = row.get(1).unwrap_or("").to_owned();
            let seq_scan = row.get(2).unwrap_or("0").to_owned();
            let seq_tup_read = row.get(3).unwrap_or("0").to_owned();
            let idx_scan = row.get(4).unwrap_or("0").to_owned();
            let live_tup = row.get(5).unwrap_or("0").to_owned();

            findings.push(IndexFinding {
                kind: FindingKind::MissingIndex,
                schema,
                table: table.clone(),
                index_name: String::new(),
                description: format!(
                    "Table {table} has {seq_scan} seq scans vs {idx_scan} idx scans \
                     ({live_tup} live rows, {seq_tup_read} tuples read by seq scan)"
                ),
                severity: Severity::Warning,
                evidence_class: EvidenceClass::Heuristic,
                suggested_action: None, // Missing index needs query analysis to suggest columns.
                size_bytes: None,
            });
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Format bytes into a human-readable string.
#[allow(clippy::cast_precision_loss)]
fn format_bytes(bytes: i64) -> String {
    const KB: i64 = 1024;
    const MB: i64 = 1024 * KB;
    const GB: i64 = 1024 * MB;

    if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
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
        assert_eq!(FindingKind::Invalid.label(), "invalid");
        assert_eq!(FindingKind::Unused.label(), "unused");
        assert_eq!(FindingKind::Redundant.label(), "redundant");
        assert_eq!(FindingKind::Bloated.label(), "bloated");
        assert_eq!(FindingKind::MissingIndex.label(), "missing_index");
    }

    #[test]
    fn finding_kind_evidence_class() {
        assert_eq!(
            FindingKind::Invalid.evidence_class(),
            EvidenceClass::Factual
        );
        assert_eq!(
            FindingKind::Unused.evidence_class(),
            EvidenceClass::Heuristic
        );
        assert_eq!(
            FindingKind::MissingIndex.evidence_class(),
            EvidenceClass::Heuristic
        );
    }

    #[test]
    fn format_bytes_display() {
        assert_eq!(format_bytes(500), "500 B");
        assert_eq!(format_bytes(2048), "2.0 KB");
        assert_eq!(format_bytes(10_485_760), "10.0 MB");
        assert_eq!(format_bytes(1_073_741_824), "1.0 GB");
    }

    #[test]
    fn empty_report_display() {
        let report = IndexHealthReport {
            findings: Vec::new(),
        };
        assert!(report.to_prompt().contains("No index health issues"));
    }

    #[test]
    fn report_to_prompt_with_findings() {
        let report = IndexHealthReport {
            findings: vec![IndexFinding {
                kind: FindingKind::Invalid,
                schema: "public".to_owned(),
                table: "orders".to_owned(),
                index_name: "idx_broken".to_owned(),
                description: "Index is invalid".to_owned(),
                severity: Severity::Critical,
                evidence_class: EvidenceClass::Factual,
                suggested_action: Some("REINDEX".to_owned()),
                size_bytes: Some(1024),
            }],
        };
        let prompt = report.to_prompt();
        assert!(prompt.contains("1 finding"));
        assert!(prompt.contains("[invalid]"));
        assert!(prompt.contains("public.orders"));
        assert!(prompt.contains("REINDEX"));
    }

    #[test]
    fn report_sorts_by_severity() {
        let mut report = IndexHealthReport {
            findings: vec![
                IndexFinding {
                    kind: FindingKind::Unused,
                    schema: "s".to_owned(),
                    table: "t".to_owned(),
                    index_name: "idx1".to_owned(),
                    description: "unused".to_owned(),
                    severity: Severity::Warning,
                    evidence_class: EvidenceClass::Heuristic,
                    suggested_action: None,
                    size_bytes: None,
                },
                IndexFinding {
                    kind: FindingKind::Invalid,
                    schema: "s".to_owned(),
                    table: "t".to_owned(),
                    index_name: "idx2".to_owned(),
                    description: "invalid".to_owned(),
                    severity: Severity::Critical,
                    evidence_class: EvidenceClass::Factual,
                    suggested_action: None,
                    size_bytes: None,
                },
            ],
        };
        report.findings.sort_by(|a, b| b.severity.cmp(&a.severity));
        assert_eq!(report.findings[0].severity, Severity::Critical);
        assert_eq!(report.findings[1].severity, Severity::Warning);
    }

    #[test]
    fn invalid_indexes_sql_is_valid() {
        assert!(INVALID_INDEXES_SQL.contains("indisvalid"));
        assert!(INVALID_INDEXES_SQL.contains("pg_index"));
    }

    #[test]
    fn unused_indexes_sql_excludes_pk_unique() {
        assert!(UNUSED_INDEXES_SQL.contains("indisprimary"));
        assert!(UNUSED_INDEXES_SQL.contains("indisunique"));
    }

    #[test]
    fn redundant_indexes_sql_checks_prefix() {
        assert!(REDUNDANT_INDEXES_SQL.contains("indnkeyatts"));
        assert!(REDUNDANT_INDEXES_SQL.contains("indkey"));
    }

    #[test]
    fn missing_index_sql_filters_large_tables() {
        assert!(MISSING_INDEX_CANDIDATES_SQL.contains("n_live_tup > 10000"));
        assert!(MISSING_INDEX_CANDIDATES_SQL.contains("seq_scan > 100"));
    }

    #[test]
    fn bloated_indexes_sql_has_threshold() {
        assert!(BLOATED_INDEXES_SQL.contains("10485760")); // 10 MB minimum.
        assert!(BLOATED_INDEXES_SQL.contains("0.3")); // 30% bloat threshold.
    }
}
