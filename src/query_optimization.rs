//! Query optimization Analyzer — detects long-running queries, idle-in-transaction
//! sessions, blocking chains, and high session counts.
//!
//! Operates at Observe level: reads `pg_stat_activity` and `pg_locks`
//! to produce structured findings. No writes are performed.
//!
//! # Sub-findings
//!
//! | Sub-finding | Evidence Class | Source |
//! |---|---|---|
//! | Long-running query (> 60s) | Factual | `pg_stat_activity` |
//! | Idle-in-transaction session (> 5min) | Factual | `pg_stat_activity` |
//! | Blocking chain | Factual | `pg_locks` + `pg_stat_activity` |
//! | High session count | Heuristic | `pg_stat_activity` |

// Phase 2/3 infrastructure — compiled but not yet wired into the main dispatch loop.
#![allow(dead_code)]

use crate::governance::{EvidenceClass, Severity};

use std::fmt::Write as _;

// ---------------------------------------------------------------------------
// Query optimization finding types
// ---------------------------------------------------------------------------

/// Category of query optimization finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryFindingKind {
    /// Query has been running for more than 60 seconds.
    LongRunningQuery,
    /// Session has been idle in transaction for more than 5 minutes.
    IdleInTransaction,
    /// A session is blocked waiting for a lock held by another session.
    BlockingChain,
    /// Total client backend session count is elevated.
    HighSessionCount,
}

impl QueryFindingKind {
    /// Evidence class for this finding kind.
    #[allow(dead_code)]
    pub fn evidence_class(self) -> EvidenceClass {
        match self {
            Self::LongRunningQuery | Self::IdleInTransaction | Self::BlockingChain => {
                EvidenceClass::Factual
            }
            Self::HighSessionCount => EvidenceClass::Heuristic,
        }
    }

    /// Human-readable label.
    pub fn label(self) -> &'static str {
        match self {
            Self::LongRunningQuery => "long_running_query",
            Self::IdleInTransaction => "idle_in_transaction",
            Self::BlockingChain => "blocking_chain",
            Self::HighSessionCount => "high_session_count",
        }
    }
}

/// A single query optimization finding.
#[derive(Debug, Clone)]
pub struct QueryFinding {
    /// What kind of finding.
    pub kind: QueryFindingKind,
    /// Process ID of the backend (0 for instance-level findings).
    pub pid: i32,
    /// Database name (empty for instance-level findings).
    pub datname: String,
    /// Username (empty for instance-level findings).
    pub usename: String,
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

/// Complete query optimization report.
#[derive(Debug, Clone)]
pub struct QueryOptimizationReport {
    /// All findings, sorted by severity (critical first).
    pub findings: Vec<QueryFinding>,
}

impl QueryOptimizationReport {
    /// Display the report to the terminal.
    pub fn display(&self) {
        if self.findings.is_empty() {
            eprintln!("Query optimization: no issues found.");
            return;
        }
        eprintln!(
            "Query optimization: {} issue{} found.\n",
            self.findings.len(),
            if self.findings.len() == 1 { "" } else { "s" }
        );
        for f in &self.findings {
            let icon = match f.severity {
                Severity::Critical => "!!",
                Severity::Warning => "! ",
                Severity::Info => "  ",
            };
            if f.pid == 0 {
                eprintln!("{icon} [{}] {}", f.kind.label(), f.description);
            } else {
                eprintln!(
                    "{icon} [{}] pid={} db={} user={}",
                    f.kind.label(),
                    f.pid,
                    f.datname,
                    f.usename,
                );
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
            return "No query optimization issues found.".to_owned();
        }
        let mut out = format!(
            "Query optimization report: {} finding(s)\n\n",
            self.findings.len()
        );
        for (i, f) in self.findings.iter().enumerate() {
            if f.pid == 0 {
                let _ = writeln!(out, "{}. [{}] {}", i + 1, f.kind.label(), f.description);
            } else {
                let _ = writeln!(
                    out,
                    "{}. [{}] pid={} db={} user={}: {}",
                    i + 1,
                    f.kind.label(),
                    f.pid,
                    f.datname,
                    f.usename,
                    f.description,
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

/// Detect queries running longer than 60 seconds.
const LONG_RUNNING_SQL: &str = "\
    select \
        pid, \
        usename, \
        datname, \
        state, \
        now() - query_start as duration, \
        left(query, 200) as query \
    from pg_stat_activity \
    where \
        state = 'active' \
        and pid != pg_backend_pid() \
        and now() - query_start > interval '60 seconds' \
    order by duration desc \
    limit 10";

/// Detect sessions idle in transaction for more than 5 minutes.
const IDLE_IN_TRANSACTION_SQL: &str = "\
    select \
        pid, \
        usename, \
        datname, \
        state, \
        now() - state_change as idle_duration, \
        left(query, 200) as query \
    from pg_stat_activity \
    where \
        state = 'idle in transaction' \
        and now() - state_change > interval '5 minutes' \
    order by idle_duration desc \
    limit 10";

/// Detect blocking chains: sessions blocked waiting for a lock.
const BLOCKING_CHAINS_SQL: &str = "\
    select \
        blocked.pid as blocked_pid, \
        blocked.usename as blocked_user, \
        left(blocked.query, 100) as blocked_query, \
        blocking.pid as blocking_pid, \
        blocking.usename as blocking_user, \
        left(blocking.query, 100) as blocking_query \
    from pg_stat_activity as blocked \
    join pg_locks as bl \
        on bl.pid = blocked.pid \
    join pg_locks as kl \
        on kl.locktype = bl.locktype \
        and kl.database is not distinct from bl.database \
        and kl.relation is not distinct from bl.relation \
        and kl.page is not distinct from bl.page \
        and kl.tuple is not distinct from bl.tuple \
        and kl.virtualxid is not distinct from bl.virtualxid \
        and kl.transactionid is not distinct from bl.transactionid \
        and kl.classid is not distinct from bl.classid \
        and kl.objid is not distinct from bl.objid \
        and kl.objsubid is not distinct from bl.objsubid \
        and kl.pid != bl.pid \
    join pg_stat_activity as blocking \
        on blocking.pid = kl.pid \
    where \
        not bl.granted \
        and kl.granted \
    limit 10";

/// Count client backend sessions by state.
const SESSION_COUNTS_SQL: &str = "\
    select \
        state, \
        count(*) \
    from pg_stat_activity \
    where backend_type = 'client backend' \
    group by state";

// ---------------------------------------------------------------------------
// Public analyzer
// ---------------------------------------------------------------------------

/// Collect query optimization findings from the database.
///
/// Runs diagnostic queries against `pg_stat_activity` and `pg_locks`.
/// All operations are read-only (Observe mode).
pub async fn analyze(client: &tokio_postgres::Client) -> QueryOptimizationReport {
    let mut findings = Vec::new();

    collect_long_running_queries(client, &mut findings).await;
    collect_idle_in_transaction(client, &mut findings).await;
    collect_blocking_chains(client, &mut findings).await;
    collect_session_counts(client, &mut findings).await;

    // Sort: Critical first, then Warning, then Info.
    findings.sort_by(|a, b| b.severity.cmp(&a.severity));

    QueryOptimizationReport { findings }
}

// ---------------------------------------------------------------------------
// Collection helpers
// ---------------------------------------------------------------------------

async fn collect_long_running_queries(
    client: &tokio_postgres::Client,
    findings: &mut Vec<QueryFinding>,
) {
    let Ok(messages) = client.simple_query(LONG_RUNNING_SQL).await else {
        return;
    };
    for msg in messages {
        if let tokio_postgres::SimpleQueryMessage::Row(row) = msg {
            let pid: i32 = row.get(0).and_then(|s| s.parse().ok()).unwrap_or(0);
            let usename = row.get(1).unwrap_or("").to_owned();
            let datname = row.get(2).unwrap_or("").to_owned();
            let duration = row.get(4).unwrap_or("unknown").to_owned();
            let query = row.get(5).unwrap_or("").to_owned();

            // Parse duration as seconds from the interval string (e.g. "00:02:30.123456").
            let secs = parse_interval_seconds(&duration);
            let severity = if secs >= 300 {
                Severity::Critical
            } else {
                Severity::Warning
            };

            findings.push(QueryFinding {
                kind: QueryFindingKind::LongRunningQuery,
                pid,
                datname,
                usename,
                description: format!("Query running for {duration}: {query}"),
                severity,
                evidence_class: EvidenceClass::Factual,
                suggested_action: Some(format!("SELECT pg_cancel_backend({pid})")),
            });
        }
    }
}

async fn collect_idle_in_transaction(
    client: &tokio_postgres::Client,
    findings: &mut Vec<QueryFinding>,
) {
    let Ok(messages) = client.simple_query(IDLE_IN_TRANSACTION_SQL).await else {
        return;
    };
    for msg in messages {
        if let tokio_postgres::SimpleQueryMessage::Row(row) = msg {
            let pid: i32 = row.get(0).and_then(|s| s.parse().ok()).unwrap_or(0);
            let usename = row.get(1).unwrap_or("").to_owned();
            let datname = row.get(2).unwrap_or("").to_owned();
            let idle_duration = row.get(4).unwrap_or("unknown").to_owned();
            let query = row.get(5).unwrap_or("").to_owned();

            let secs = parse_interval_seconds(&idle_duration);
            let severity = if secs >= 1800 {
                Severity::Critical
            } else {
                Severity::Warning
            };

            findings.push(QueryFinding {
                kind: QueryFindingKind::IdleInTransaction,
                pid,
                datname,
                usename,
                description: format!(
                    "Session idle in transaction for {idle_duration}; \
                     last query: {query}"
                ),
                severity,
                evidence_class: EvidenceClass::Factual,
                suggested_action: Some(format!("SELECT pg_terminate_backend({pid})")),
            });
        }
    }
}

async fn collect_blocking_chains(
    client: &tokio_postgres::Client,
    findings: &mut Vec<QueryFinding>,
) {
    let Ok(messages) = client.simple_query(BLOCKING_CHAINS_SQL).await else {
        return;
    };
    for msg in messages {
        if let tokio_postgres::SimpleQueryMessage::Row(row) = msg {
            let blocked_pid: i32 = row.get(0).and_then(|s| s.parse().ok()).unwrap_or(0);
            let blocked_user = row.get(1).unwrap_or("").to_owned();
            let blocked_query = row.get(2).unwrap_or("").to_owned();
            let blocking_pid: i32 = row.get(3).and_then(|s| s.parse().ok()).unwrap_or(0);
            let blocking_user = row.get(4).unwrap_or("").to_owned();
            let blocking_query = row.get(5).unwrap_or("").to_owned();

            findings.push(QueryFinding {
                kind: QueryFindingKind::BlockingChain,
                pid: blocked_pid,
                datname: String::new(),
                usename: blocked_user.clone(),
                description: format!(
                    "pid {blocked_pid} ({blocked_user}) blocked by \
                     pid {blocking_pid} ({blocking_user}); \
                     blocked query: {blocked_query}; \
                     blocking query: {blocking_query}"
                ),
                severity: Severity::Critical,
                evidence_class: EvidenceClass::Factual,
                suggested_action: Some(format!(
                    "SELECT pg_cancel_backend({blocking_pid}) \
                     -- or pg_terminate_backend({blocking_pid}) if cancel is not enough"
                )),
            });
        }
    }
}

async fn collect_session_counts(client: &tokio_postgres::Client, findings: &mut Vec<QueryFinding>) {
    let Ok(messages) = client.simple_query(SESSION_COUNTS_SQL).await else {
        return;
    };

    let mut total: i64 = 0;
    let mut idle_in_txn: i64 = 0;

    for msg in messages {
        if let tokio_postgres::SimpleQueryMessage::Row(row) = msg {
            let state = row.get(0).unwrap_or("").to_owned();
            let count: i64 = row.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
            total += count;
            if state == "idle in transaction" {
                idle_in_txn += count;
            }
        }
    }

    if total == 0 {
        return;
    }

    let severity = if total >= 400 {
        Severity::Critical
    } else if total >= 200 {
        Severity::Warning
    } else {
        Severity::Info
    };

    let mut desc = format!("{total} client backend session(s) total");
    if idle_in_txn > 0 {
        let _ = write!(desc, ", {idle_in_txn} idle in transaction");
    }

    findings.push(QueryFinding {
        kind: QueryFindingKind::HighSessionCount,
        pid: 0,
        datname: String::new(),
        usename: String::new(),
        description: desc,
        severity,
        evidence_class: EvidenceClass::Heuristic,
        suggested_action: if total >= 200 {
            Some("Review pg_stat_activity; consider connection pooling (e.g. PgBouncer)".to_owned())
        } else {
            None
        },
    });
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Parse a `PostgreSQL` interval string (e.g. `"00:05:30.123456"` or
/// `"1 day 02:00:00"`) into total seconds.
///
/// Returns 0 on parse failure, which is safe — it just means we treat
/// an unparseable duration as below threshold.
fn parse_interval_seconds(interval: &str) -> u64 {
    let mut total: u64 = 0;

    // Handle "N day(s)" prefix (e.g. "1 day 02:00:00" or "2 days 00:00:00").
    let time_part = if let Some(idx) = interval.find("day") {
        // Extract the number of days before "day".
        let before = interval[..idx].trim();
        let days: u64 = before
            .split_whitespace()
            .next_back()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        total += days * 86_400;
        // The time part follows the "days " token.
        let after_day = &interval[idx..];
        // Skip past "day" or "days" and any trailing whitespace.
        after_day
            .split_once(char::is_whitespace)
            .map_or("", |x| x.1)
            .trim()
    } else {
        interval.trim()
    };

    // Parse "HH:MM:SS[.fraction]".
    let mut parts = time_part.splitn(3, ':');
    let h: u64 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let m: u64 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    // Strip sub-second fraction before parsing seconds.
    let s: u64 = parts
        .next()
        .and_then(|s| s.split('.').next())
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    total += h * 3600 + m * 60 + s;
    total
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
            QueryFindingKind::LongRunningQuery.label(),
            "long_running_query"
        );
        assert_eq!(
            QueryFindingKind::IdleInTransaction.label(),
            "idle_in_transaction"
        );
        assert_eq!(QueryFindingKind::BlockingChain.label(), "blocking_chain");
        assert_eq!(
            QueryFindingKind::HighSessionCount.label(),
            "high_session_count"
        );
    }

    #[test]
    fn finding_kind_evidence_classes() {
        assert_eq!(
            QueryFindingKind::LongRunningQuery.evidence_class(),
            EvidenceClass::Factual
        );
        assert_eq!(
            QueryFindingKind::IdleInTransaction.evidence_class(),
            EvidenceClass::Factual
        );
        assert_eq!(
            QueryFindingKind::BlockingChain.evidence_class(),
            EvidenceClass::Factual
        );
        assert_eq!(
            QueryFindingKind::HighSessionCount.evidence_class(),
            EvidenceClass::Heuristic
        );
    }

    #[test]
    fn empty_report_display_message() {
        let report = QueryOptimizationReport {
            findings: Vec::new(),
        };
        assert!(report.to_prompt().contains("No query optimization issues"));
    }

    #[test]
    fn report_to_prompt_with_findings() {
        let report = QueryOptimizationReport {
            findings: vec![QueryFinding {
                kind: QueryFindingKind::LongRunningQuery,
                pid: 1234,
                datname: "mydb".to_owned(),
                usename: "alice".to_owned(),
                description: "Query running for 00:02:30: select ...".to_owned(),
                severity: Severity::Warning,
                evidence_class: EvidenceClass::Factual,
                suggested_action: Some("SELECT pg_cancel_backend(1234)".to_owned()),
            }],
        };
        let prompt = report.to_prompt();
        assert!(prompt.contains("1 finding"));
        assert!(prompt.contains("[long_running_query]"));
        assert!(prompt.contains("pid=1234"));
        assert!(prompt.contains("pg_cancel_backend"));
    }

    #[test]
    fn report_sorts_by_severity() {
        let mut report = QueryOptimizationReport {
            findings: vec![
                QueryFinding {
                    kind: QueryFindingKind::HighSessionCount,
                    pid: 0,
                    datname: String::new(),
                    usename: String::new(),
                    description: "50 sessions".to_owned(),
                    severity: Severity::Info,
                    evidence_class: EvidenceClass::Heuristic,
                    suggested_action: None,
                },
                QueryFinding {
                    kind: QueryFindingKind::BlockingChain,
                    pid: 42,
                    datname: String::new(),
                    usename: "bob".to_owned(),
                    description: "blocked".to_owned(),
                    severity: Severity::Critical,
                    evidence_class: EvidenceClass::Factual,
                    suggested_action: None,
                },
                QueryFinding {
                    kind: QueryFindingKind::LongRunningQuery,
                    pid: 99,
                    datname: "db".to_owned(),
                    usename: "alice".to_owned(),
                    description: "running 2min".to_owned(),
                    severity: Severity::Warning,
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
    fn parse_interval_hms() {
        assert_eq!(parse_interval_seconds("00:01:30"), 90);
        assert_eq!(parse_interval_seconds("01:00:00"), 3600);
        assert_eq!(parse_interval_seconds("00:05:00"), 300);
        assert_eq!(parse_interval_seconds("00:00:00"), 0);
    }

    #[test]
    fn parse_interval_with_fraction() {
        assert_eq!(parse_interval_seconds("00:02:30.123456"), 150);
        assert_eq!(parse_interval_seconds("00:10:00.000001"), 600);
    }

    #[test]
    fn parse_interval_with_days() {
        assert_eq!(parse_interval_seconds("1 day 00:00:00"), 86_400);
        assert_eq!(parse_interval_seconds("2 days 01:00:00"), 2 * 86_400 + 3600);
    }

    #[test]
    fn parse_interval_invalid_returns_zero() {
        assert_eq!(parse_interval_seconds("unknown"), 0);
        assert_eq!(parse_interval_seconds(""), 0);
    }

    #[test]
    fn long_running_query_warning_at_60s() {
        // 90 seconds → Warning (< 300s threshold for Critical).
        let secs = parse_interval_seconds("00:01:30");
        let severity = if secs >= 300 {
            Severity::Critical
        } else {
            Severity::Warning
        };
        assert_eq!(severity, Severity::Warning);
    }

    #[test]
    fn long_running_query_critical_at_5min() {
        // 5 minutes exactly → Critical.
        let secs = parse_interval_seconds("00:05:00");
        let severity = if secs >= 300 {
            Severity::Critical
        } else {
            Severity::Warning
        };
        assert_eq!(severity, Severity::Critical);
    }

    #[test]
    fn idle_in_transaction_warning_at_5min() {
        let secs = parse_interval_seconds("00:05:00");
        let severity = if secs >= 1800 {
            Severity::Critical
        } else {
            Severity::Warning
        };
        assert_eq!(severity, Severity::Warning);
    }

    #[test]
    fn idle_in_transaction_critical_at_30min() {
        let secs = parse_interval_seconds("00:30:00");
        let severity = if secs >= 1800 {
            Severity::Critical
        } else {
            Severity::Warning
        };
        assert_eq!(severity, Severity::Critical);
    }

    #[test]
    fn high_session_count_info_below_threshold() {
        let total: i64 = 100;
        let severity = if total >= 400 {
            Severity::Critical
        } else if total >= 200 {
            Severity::Warning
        } else {
            Severity::Info
        };
        assert_eq!(severity, Severity::Info);
    }

    #[test]
    fn high_session_count_warning_at_200() {
        let total: i64 = 250;
        let severity = if total >= 400 {
            Severity::Critical
        } else if total >= 200 {
            Severity::Warning
        } else {
            Severity::Info
        };
        assert_eq!(severity, Severity::Warning);
    }

    #[test]
    fn high_session_count_critical_at_400() {
        let total: i64 = 400;
        let severity = if total >= 400 {
            Severity::Critical
        } else if total >= 200 {
            Severity::Warning
        } else {
            Severity::Info
        };
        assert_eq!(severity, Severity::Critical);
    }

    #[test]
    fn long_running_sql_filters_active_and_self() {
        assert!(LONG_RUNNING_SQL.contains("state = 'active'"));
        assert!(LONG_RUNNING_SQL.contains("pg_backend_pid()"));
        assert!(LONG_RUNNING_SQL.contains("interval '60 seconds'"));
    }

    #[test]
    fn idle_in_transaction_sql_correct_state() {
        assert!(IDLE_IN_TRANSACTION_SQL.contains("idle in transaction"));
        assert!(IDLE_IN_TRANSACTION_SQL.contains("interval '5 minutes'"));
    }

    #[test]
    fn blocking_chains_sql_uses_pg_locks() {
        assert!(BLOCKING_CHAINS_SQL.contains("pg_locks"));
        assert!(BLOCKING_CHAINS_SQL.contains("not bl.granted"));
        assert!(BLOCKING_CHAINS_SQL.contains("kl.granted"));
    }

    #[test]
    fn session_counts_sql_filters_client_backends() {
        assert!(SESSION_COUNTS_SQL.contains("client backend"));
        assert!(SESSION_COUNTS_SQL.contains("backend_type"));
    }

    #[test]
    fn blocking_chain_is_always_critical() {
        let finding = QueryFinding {
            kind: QueryFindingKind::BlockingChain,
            pid: 10,
            datname: String::new(),
            usename: "user".to_owned(),
            description: "blocked".to_owned(),
            severity: Severity::Critical,
            evidence_class: EvidenceClass::Factual,
            suggested_action: None,
        };
        assert_eq!(finding.severity, Severity::Critical);
    }
}
