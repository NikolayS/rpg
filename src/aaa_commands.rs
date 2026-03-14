//! Formatting helpers for `\aaa` governance status REPL commands.
//!
//! Each public function takes governance state and returns a `String`
//! ready for display in the interactive REPL.  REPL wiring (dispatch,
//! command parsing) will be added in a follow-up PR; the module is
//! compiled but not yet wired into the main dispatch loop.

// REPL wiring arrives in a follow-up PR; suppress dead_code for now.
#![allow(dead_code)]

use std::collections::HashMap;
use std::fmt::Write as _;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::dispatcher::{Dispatcher, PromotionStatus};
use crate::governance::{
    ActionOutcome, AuditLog, AuditLogEntry, AutonomyLevel, FeatureArea, VetoTracker,
};

// ---------------------------------------------------------------------------
// Internal timestamp helpers
// ---------------------------------------------------------------------------

/// Format a [`SystemTime`] as `YYYY-MM-DD HH:mm:ss` (UTC, no sub-seconds).
///
/// Uses only `std` — avoids a `chrono` dependency.
fn format_timestamp(t: SystemTime) -> String {
    let secs = t
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs();

    // Calendar decomposition — good for years 1970-2099.
    let s = secs % 60;
    let m = (secs / 60) % 60;
    let h = (secs / 3600) % 24;
    let days = secs / 86400;

    // Days since 1970-01-01 → (year, month, day).
    let (year, month, day) = days_to_ymd(days);

    format!("{year:04}-{month:02}-{day:02} {h:02}:{m:02}:{s:02}")
}

/// Convert a count of days since the Unix epoch to `(year, month, day)`.
///
/// Handles leap years correctly for the range 1970–2099 (sufficient for
/// audit log timestamps in this project's lifetime).
fn days_to_ymd(mut days: u64) -> (u64, u64, u64) {
    // Leap year predicate — valid for 1970-2099 (no century adjustment needed
    // until 2100, which is not a leap year, but that's 74 years away).
    let is_leap = |y: u64| y % 4 == 0;

    let mut year = 1970u64;
    loop {
        let days_in_year = if is_leap(year) { 366 } else { 365 };
        if days < days_in_year {
            break;
        }
        days -= days_in_year;
        year += 1;
    }

    let month_days: [u64; 12] = if is_leap(year) {
        [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    } else {
        [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    };

    let mut month = 1u64;
    for &md in &month_days {
        if days < md {
            break;
        }
        days -= md;
        month += 1;
    }

    (year, month, days + 1)
}

// ---------------------------------------------------------------------------
// Outcome label
// ---------------------------------------------------------------------------

fn outcome_label(outcome: &ActionOutcome) -> &'static str {
    match outcome {
        ActionOutcome::Success { .. } => "Success",
        ActionOutcome::Failure { .. } => "Failure",
        ActionOutcome::Vetoed { .. } => "Vetoed",
        ActionOutcome::Skipped => "Skipped",
    }
}

// ---------------------------------------------------------------------------
// 1. format_aaa_status — overview of all governance state
// ---------------------------------------------------------------------------

/// Render a tabular overview of all feature areas showing autonomy level,
/// circuit-breaker state, and Auto-promotion progress.
///
/// # Example output
///
/// ```text
/// === AAA Governance Status ===
///
/// Feature              Autonomy   Breaker   Promotion
/// ─────────────────────────────────────────────────────────
/// vacuum               observe    ok        -
/// bloat                observe    ok        -
/// index_health         supervised ok        →A 12/30 (85%)
/// rca                  auto       ok        eligible
/// ```
pub fn format_aaa_status(
    dispatcher: &Dispatcher,
    current_levels: &HashMap<FeatureArea, AutonomyLevel>,
) -> String {
    let breaker_states = dispatcher.circuit_breaker_states();
    let breaker_map: HashMap<FeatureArea, (bool, f64)> = breaker_states
        .into_iter()
        .map(|(f, tripped, rate)| (f, (tripped, rate)))
        .collect();

    let promotion_statuses: HashMap<FeatureArea, PromotionStatus> = dispatcher
        .promotion_status(current_levels)
        .into_iter()
        .collect();

    let mut out = String::new();
    out.push_str("=== AAA Governance Status ===\n\n");

    let header = format!(
        "{:<20} {:<10} {:<9} {}",
        "Feature", "Autonomy", "Breaker", "Promotion"
    );
    let separator = "\u{2500}".repeat(57);

    out.push_str(&header);
    out.push('\n');
    out.push_str(&separator);
    out.push('\n');

    for &feature in FeatureArea::all() {
        let autonomy = current_levels
            .get(&feature)
            .copied()
            .unwrap_or(AutonomyLevel::Observe);

        let breaker_label = match breaker_map.get(&feature) {
            Some((true, _)) => "tripped",
            _ => "ok",
        };

        let promotion_label = if let Some(status) = promotion_statuses.get(&feature) {
            if status.eligible_for_next {
                "eligible".to_owned()
            } else if autonomy == AutonomyLevel::Supervised {
                let pct = format!("{:.0}", status.approval_rate * 100.0);
                format!(
                    "\u{2192}A {}/{} ({}%)",
                    status.successful_actions, status.required_actions, pct
                )
            } else {
                "-".to_owned()
            }
        } else {
            "-".to_owned()
        };

        let _ = writeln!(
            out,
            "{:<20} {:<10} {:<9} {}",
            feature.label(),
            autonomy.label(),
            breaker_label,
            promotion_label,
        );
    }

    out
}

// ---------------------------------------------------------------------------
// 2. format_audit_log — last N audit log entries
// ---------------------------------------------------------------------------

/// Render the last `count` entries from the audit log as a table.
///
/// Entries are shown most-recent-first (reverse chronological order).
///
/// # Example output
///
/// ```text
/// Seq  Time                 Feature       Level       Action               Outcome
/// ────────────────────────────────────────────────────────────────────────────────
///  42  2026-03-14 10:15:22  rca           auto        pg_cancel(123)       Success
///  41  2026-03-14 10:14:58  index_health  supervised  REINDEX idx_foo      Skipped
/// ```
pub fn format_audit_log(log: &AuditLog, count: usize) -> String {
    let entries: Vec<&AuditLogEntry> = log.entries().iter().rev().take(count).collect();

    let mut out = String::new();

    let header = format!(
        "{:<4} {:<20} {:<13} {:<11} {:<20} {}",
        "Seq", "Time", "Feature", "Level", "Action", "Outcome"
    );
    let separator = "\u{2500}".repeat(82);

    out.push_str(&header);
    out.push('\n');
    out.push_str(&separator);
    out.push('\n');

    if entries.is_empty() {
        out.push_str("(no entries)\n");
        return out;
    }

    for entry in entries {
        let action_truncated = truncate(&entry.action, 19);
        let _ = writeln!(
            out,
            "{:<4} {:<20} {:<13} {:<11} {:<20} {}",
            entry.seq,
            format_timestamp(entry.timestamp),
            entry.feature.label(),
            entry.autonomy_level.label(),
            action_truncated,
            outcome_label(&entry.outcome),
        );
    }

    out
}

// ---------------------------------------------------------------------------
// 3. format_vetoes — active veto patterns
// ---------------------------------------------------------------------------

/// Render the current veto patterns from a [`VetoTracker`] as a table.
///
/// # Example output
///
/// ```text
/// Feature          Action Pattern              Vetoed Since
/// ─────────────────────────────────────────────────────────
/// rca              pg_terminate(%)             2026-03-14 10:12:00
/// ```
///
/// Because the [`VetoTracker`] does not record when each veto was added,
/// the "Vetoed Since" column is populated from the *first* matching audit
/// log entry (if provided).  Pass `None` to omit that column.
pub fn format_vetoes(tracker: &VetoTracker, log: Option<&AuditLog>) -> String {
    let vetoes = tracker.vetoes();

    let mut out = String::new();

    let header = if log.is_some() {
        format!(
            "{:<16} {:<27} {}",
            "Feature", "Action Pattern", "Vetoed Since"
        )
    } else {
        format!("{:<16} {}", "Feature", "Action Pattern")
    };
    let separator = "\u{2500}".repeat(if log.is_some() { 65 } else { 44 });

    out.push_str(&header);
    out.push('\n');
    out.push_str(&separator);
    out.push('\n');

    if vetoes.is_empty() {
        out.push_str("(no active vetoes)\n");
        return out;
    }

    for (feature, pattern) in vetoes {
        if let Some(audit_log) = log {
            // Find the first matching vetoed entry for a "since" timestamp.
            let since = audit_log
                .entries()
                .iter()
                .find(|e| {
                    e.feature == *feature
                        && matches!(e.outcome, ActionOutcome::Vetoed { .. })
                        && e.action.to_lowercase().contains(pattern.as_str())
                })
                .map_or_else(|| "-".to_owned(), |e| format_timestamp(e.timestamp));

            let _ = writeln!(out, "{:<16} {:<27} {}", feature.label(), pattern, since,);
        } else {
            let _ = writeln!(out, "{:<16} {}", feature.label(), pattern,);
        }
    }

    out
}

// ---------------------------------------------------------------------------
// 4. format_breaker_status — circuit breaker state per feature
// ---------------------------------------------------------------------------

/// Render circuit breaker state for all features known to the dispatcher.
///
/// # Example output
///
/// ```text
/// Feature              State     Failure Rate
/// ────────────────────────────────────────────
/// vacuum               ok        0%
/// index_health         tripped   18%
/// ```
pub fn format_breaker_status(dispatcher: &Dispatcher) -> String {
    let mut states = dispatcher.circuit_breaker_states();

    // Sort by feature label for stable output.
    states.sort_by_key(|(f, _, _)| f.label());

    let mut out = String::new();

    let header = format!("{:<20} {:<9} {}", "Feature", "State", "Failure Rate");
    let separator = "\u{2500}".repeat(44);

    out.push_str(&header);
    out.push('\n');
    out.push_str(&separator);
    out.push('\n');

    if states.is_empty() {
        out.push_str("(no circuit breaker data yet)\n");
        return out;
    }

    for (feature, tripped, failure_rate) in states {
        let state_label = if tripped { "tripped" } else { "ok" };
        let rate_pct = format!("{:.0}%", failure_rate * 100.0);
        let _ = writeln!(
            out,
            "{:<20} {:<9} {}",
            feature.label(),
            state_label,
            rate_pct,
        );
    }

    out
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Truncate a string to at most `max_chars` characters, appending `…` if
/// truncated.
fn truncate(s: &str, max_chars: usize) -> String {
    let mut chars = s.chars();
    let collected: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{collected}\u{2026}")
    } else {
        collected
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::time::{Duration, UNIX_EPOCH};

    use super::*;
    use crate::dispatcher::Dispatcher;
    use crate::governance::{
        ActionProposal, AuditLog, AutonomyLevel, EvidenceClass, FeatureArea, Severity, VetoTracker,
    };

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    fn make_proposal(feature: FeatureArea, action: &str, finding: &str) -> ActionProposal {
        ActionProposal {
            feature,
            severity: Severity::Warning,
            evidence_class: EvidenceClass::Factual,
            finding: finding.to_owned(),
            proposed_action: action.to_owned(),
            expected_outcome: "ok".to_owned(),
            risk: "low".to_owned(),
            created_at: std::time::SystemTime::now(),
        }
    }

    fn all_observe() -> HashMap<FeatureArea, AutonomyLevel> {
        FeatureArea::all()
            .iter()
            .map(|&f| (f, AutonomyLevel::Observe))
            .collect()
    }

    // -----------------------------------------------------------------------
    // 1. format_timestamp round-trip
    // -----------------------------------------------------------------------

    #[test]
    fn timestamp_known_epoch() {
        // 2026-03-14 00:00:00 UTC
        // Days from 1970-01-01 to 2026-03-14:
        //   56 complete years (1970-2025) + 72 days into 2026.
        //   Leap years in 1970-2025: 1972,1976,…,2024 → 14 leap years.
        //   Total = 56*365 + 14 + 72 = 20440 + 14 + 72 = 20526 days
        let secs: u64 = 20526 * 86_400;
        let t = UNIX_EPOCH + Duration::from_secs(secs);
        let s = format_timestamp(t);
        assert_eq!(s, "2026-03-14 00:00:00");
    }

    // -----------------------------------------------------------------------
    // 2. format_aaa_status — all Observe, no data → dashes in promotion col
    // -----------------------------------------------------------------------

    #[test]
    fn aaa_status_all_observe_no_data() {
        let d = Dispatcher::new();
        let levels = all_observe();
        let output = format_aaa_status(&d, &levels);

        assert!(output.contains("=== AAA Governance Status ==="));
        assert!(output.contains("vacuum"));
        assert!(output.contains("observe"));
        // No breaker data → all "ok".
        assert!(output.contains("ok"));
        // No promotion history → all dashes.
        assert!(output.contains('-'));
    }

    // -----------------------------------------------------------------------
    // 3. format_aaa_status — breaker state is reachable via public API
    // -----------------------------------------------------------------------

    #[test]
    fn aaa_status_breaker_state_reachable() {
        let mut d = Dispatcher::new();
        let mut levels = all_observe();
        levels.insert(FeatureArea::Vacuum, AutonomyLevel::Auto);

        // Dispatch a successful proposal; the CB entry is created.
        let p = make_proposal(FeatureArea::Vacuum, "VACUUM ANALYZE users", "dead tuples");
        d.dispatch_proposal(&p, AutonomyLevel::Auto);

        let output = format_aaa_status(&d, &levels);
        assert!(output.contains("vacuum"));
        // No failures → "ok".
        assert!(output.contains("ok"));
    }

    // -----------------------------------------------------------------------
    // 4. format_audit_log — empty log shows "(no entries)"
    // -----------------------------------------------------------------------

    #[test]
    fn audit_log_empty() {
        let log = AuditLog::new();
        let output = format_audit_log(&log, 10);
        assert!(output.contains("(no entries)"));
    }

    // -----------------------------------------------------------------------
    // 5. format_audit_log — entries shown in reverse order
    // -----------------------------------------------------------------------

    #[test]
    fn audit_log_reverse_order() {
        let mut d = Dispatcher::new();
        let p1 = make_proposal(FeatureArea::Vacuum, "VACUUM ANALYZE users", "dead tuples");
        let p2 = make_proposal(FeatureArea::Rca, "pg_cancel_backend(1)", "long query");
        d.dispatch_proposal(&p1, AutonomyLevel::Auto);
        d.dispatch_proposal(&p2, AutonomyLevel::Auto);

        let output = format_audit_log(d.audit_log(), 10);
        // seq 1 (rca) should appear before seq 0 (vacuum) in the output.
        let pos_rca = output.find("rca").unwrap_or(usize::MAX);
        let pos_vacuum = output.find("vacuum").unwrap_or(usize::MAX);
        // rca (seq 1) is more recent and should appear first (lower offset).
        assert!(
            pos_rca < pos_vacuum,
            "most recent entry must appear first; rca at {pos_rca}, vacuum at {pos_vacuum}"
        );
    }

    // -----------------------------------------------------------------------
    // 6. format_audit_log — count limits output
    // -----------------------------------------------------------------------

    #[test]
    fn audit_log_count_limits_entries() {
        let mut d = Dispatcher::new();
        for i in 0..5u32 {
            let action = format!("VACUUM ANALYZE t{i}");
            let p = make_proposal(FeatureArea::Vacuum, &action, "dead tuples");
            d.dispatch_proposal(&p, AutonomyLevel::Auto);
        }
        let output = format_audit_log(d.audit_log(), 2);
        // Count occurrences of "vacuum" (one per data row).
        let count = output.matches("vacuum").count();
        assert_eq!(count, 2, "expected exactly 2 vacuum entries, got {count}");
    }

    // -----------------------------------------------------------------------
    // 7. format_vetoes — empty tracker shows "(no active vetoes)"
    // -----------------------------------------------------------------------

    #[test]
    fn vetoes_empty() {
        let tracker = VetoTracker::new();
        let output = format_vetoes(&tracker, None);
        assert!(output.contains("(no active vetoes)"));
    }

    // -----------------------------------------------------------------------
    // 8. format_vetoes — recorded veto appears in output
    // -----------------------------------------------------------------------

    #[test]
    fn vetoes_single_entry() {
        let mut tracker = VetoTracker::new();
        tracker.record_veto(FeatureArea::Rca, "pg_terminate(%)");
        let output = format_vetoes(&tracker, None);
        assert!(output.contains("rca"));
        assert!(output.contains("pg_terminate(%)"));
    }

    // -----------------------------------------------------------------------
    // 9. format_breaker_status — no data shows placeholder
    // -----------------------------------------------------------------------

    #[test]
    fn breaker_status_no_data() {
        let d = Dispatcher::new();
        let output = format_breaker_status(&d);
        assert!(output.contains("(no circuit breaker data yet)"));
    }

    // -----------------------------------------------------------------------
    // 10. format_breaker_status — after dispatching shows feature
    // -----------------------------------------------------------------------

    #[test]
    fn breaker_status_after_dispatch() {
        let mut d = Dispatcher::new();
        let p = make_proposal(FeatureArea::Rca, "pg_cancel_backend(1)", "long query");
        d.dispatch_proposal(&p, AutonomyLevel::Auto);
        let output = format_breaker_status(&d);
        assert!(output.contains("rca"));
        // All successes → failure rate 0% → "ok".
        assert!(output.contains("ok"));
        assert!(output.contains("0%"));
    }

    // -----------------------------------------------------------------------
    // 11. truncate helper — short string is unchanged
    // -----------------------------------------------------------------------

    #[test]
    fn truncate_short_unchanged() {
        assert_eq!(truncate("hello", 10), "hello");
    }

    // -----------------------------------------------------------------------
    // 12. truncate helper — long string gets ellipsis
    // -----------------------------------------------------------------------

    #[test]
    fn truncate_long_gets_ellipsis() {
        let result = truncate("abcdefghij", 5);
        assert!(result.ends_with('\u{2026}'), "expected ellipsis at end");
        // The body before the ellipsis is 5 chars.
        let body: String = result.chars().take(5).collect();
        assert_eq!(body, "abcde");
    }

    // -----------------------------------------------------------------------
    // 13. format_aaa_status — supervised feature shows promotion progress
    // -----------------------------------------------------------------------

    #[test]
    fn aaa_status_supervised_shows_promotion() {
        let mut d = Dispatcher::new();

        // Dispatch 5 Supervised proposals; the default threshold is 30, so
        // the feature will not be eligible yet but will show progress (→A).
        for _ in 0..5 {
            let p = make_proposal(
                FeatureArea::IndexHealth,
                "REINDEX CONCURRENTLY idx",
                "unused index",
            );
            d.dispatch_proposal(&p, AutonomyLevel::Supervised);
        }

        let mut levels = all_observe();
        levels.insert(FeatureArea::IndexHealth, AutonomyLevel::Supervised);
        let output = format_aaa_status(&d, &levels);

        assert!(output.contains("index_health"));
        // Should show →A style promotion progress (5/30 with 100%).
        assert!(output.contains('\u{2192}'), "expected promotion arrow →");
    }

    // -----------------------------------------------------------------------
    // 14. format_vetoes — with log provides "Vetoed Since" column header
    // -----------------------------------------------------------------------

    #[test]
    fn vetoes_with_log_has_since_column() {
        let mut tracker = VetoTracker::new();
        tracker.record_veto(FeatureArea::Vacuum, "VACUUM FULL");
        let log = AuditLog::new();
        let output = format_vetoes(&tracker, Some(&log));
        assert!(output.contains("Vetoed Since"));
    }

    // -----------------------------------------------------------------------
    // 15. format_audit_log — outcome labels render correctly
    // -----------------------------------------------------------------------

    #[test]
    fn audit_log_outcome_labels() {
        let mut d = Dispatcher::new();
        // Auto + Factual → Success.
        let p_success = make_proposal(FeatureArea::Rca, "pg_cancel_backend(1)", "long query");
        d.dispatch_proposal(&p_success, AutonomyLevel::Auto);
        // Observe → Skipped.
        let p_skip = make_proposal(FeatureArea::Vacuum, "VACUUM ANALYZE users", "dead tuples");
        d.dispatch_proposal(&p_skip, AutonomyLevel::Observe);

        let output = format_audit_log(d.audit_log(), 10);
        assert!(output.contains("Success"), "expected Success label");
        assert!(output.contains("Skipped"), "expected Skipped label");
    }
}
