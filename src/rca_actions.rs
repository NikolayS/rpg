//! RCA Supervised mode — propose actionable mitigations.
//!
//! Bridges the RCA diagnostic snapshot to the Actor component by
//! extracting structured `ActionProposal`s from the current database
//! state.  In Supervised mode these are presented to the user for
//! approval before execution.
//!
//! All proposals are derived from direct database queries (not from
//! parsing LLM output), ensuring deterministic and auditable behavior.

use crate::actor::{ActionRequest, ActionType, Actor};
use crate::governance::{
    ActionOutcome, ActionProposal, AuditDecision, AuditLog, Auditor, AutonomyLevel, EvidenceClass,
    FeatureArea, Severity,
};
use tokio_postgres::Client;

use std::fmt::Write as _;

// ---------------------------------------------------------------------------
// Proposal extraction from live database state
// ---------------------------------------------------------------------------

/// SQL to find root blockers (PIDs blocking other sessions).
const ROOT_BLOCKERS_SQL: &str = "\
    SELECT \
        blocking.pid AS blocker_pid, \
        blocking.usename, \
        blocking.state, \
        blocking.query, \
        now() - blocking.query_start AS query_duration, \
        count(blocked.pid) AS blocked_count \
    FROM pg_stat_activity AS blocking \
    JOIN pg_stat_activity AS blocked \
        ON blocking.pid = ANY(pg_blocking_pids(blocked.pid)) \
    WHERE blocking.pid != pg_backend_pid() \
    GROUP BY blocking.pid, blocking.usename, blocking.state, \
             blocking.query, blocking.query_start \
    ORDER BY blocked_count DESC \
    LIMIT 5";

/// SQL to find long-running idle-in-transaction sessions.
const IDLE_IN_TRANSACTION_SQL: &str = "\
    SELECT \
        pid, \
        usename, \
        now() - state_change AS idle_duration, \
        left(query, 120) AS last_query \
    FROM pg_stat_activity \
    WHERE state = 'idle in transaction' \
      AND pid != pg_backend_pid() \
      AND now() - state_change > interval '5 minutes' \
    ORDER BY state_change \
    LIMIT 5";

/// SQL to check if `idle_in_transaction_session_timeout` is unset.
const IDLE_TIMEOUT_CHECK_SQL: &str = "\
    SELECT setting \
    FROM pg_settings \
    WHERE name = 'idle_in_transaction_session_timeout'";

/// SQL to check if `statement_timeout` is unset.
const STATEMENT_TIMEOUT_CHECK_SQL: &str = "\
    SELECT setting \
    FROM pg_settings \
    WHERE name = 'statement_timeout'";

/// Extract actionable proposals by querying the live database state.
///
/// Returns a list of proposals that can be routed through the Auditor
/// and presented to the user for approval.
pub async fn propose_mitigations(client: &Client) -> Vec<ActionProposal> {
    let mut proposals = Vec::new();

    // 1. Propose cancelling root blockers.
    propose_blocker_actions(client, &mut proposals).await;

    // 2. Propose terminating long idle-in-transaction sessions.
    propose_idle_termination(client, &mut proposals).await;

    // 3. Propose GUC safety nets.
    propose_timeout_gucs(client, &mut proposals).await;

    proposals
}

async fn propose_blocker_actions(client: &Client, proposals: &mut Vec<ActionProposal>) {
    let Ok(messages) = client.simple_query(ROOT_BLOCKERS_SQL).await else {
        return;
    };
    for msg in messages {
        if let tokio_postgres::SimpleQueryMessage::Row(row) = msg {
            let pid: i32 = row.get(0).and_then(|s| s.parse().ok()).unwrap_or_default();
            let user = row.get(1).unwrap_or("?");
            let state = row.get(2).unwrap_or("?");
            let query = row.get(3).unwrap_or("?");
            let duration = row.get(4).unwrap_or("?");
            let blocked_count: i32 = row.get(5).and_then(|s| s.parse().ok()).unwrap_or_default();

            if blocked_count < 1 {
                continue;
            }

            let severity = if blocked_count >= 5 {
                Severity::Critical
            } else {
                Severity::Warning
            };

            let mut finding = format!(
                "PID {pid} ({user}) is blocking {blocked_count} session(s). \
                 State: {state}, running for {duration}."
            );
            if query.len() > 1 {
                let _ = write!(finding, " Query: {}", truncate(query, 80));
            }

            proposals.push(ActionProposal {
                feature: FeatureArea::Rca,
                severity,
                evidence_class: EvidenceClass::Factual,
                finding,
                proposed_action: format!("SELECT pg_cancel_backend({pid})"),
                expected_outcome: format!(
                    "Cancel blocking query on PID {pid}, unblocking {blocked_count} session(s)"
                ),
                risk: "The cancelled query will receive an error. \
                       The application may retry."
                    .to_owned(),
                created_at: std::time::SystemTime::now(),
            });
        }
    }
}

async fn propose_idle_termination(client: &Client, proposals: &mut Vec<ActionProposal>) {
    let Ok(messages) = client.simple_query(IDLE_IN_TRANSACTION_SQL).await else {
        return;
    };
    for msg in messages {
        if let tokio_postgres::SimpleQueryMessage::Row(row) = msg {
            let pid: i32 = row.get(0).and_then(|s| s.parse().ok()).unwrap_or_default();
            let user = row.get(1).unwrap_or("?");
            let duration = row.get(2).unwrap_or("?");
            let last_query = row.get(3).unwrap_or("?");

            proposals.push(ActionProposal {
                feature: FeatureArea::Rca,
                severity: Severity::Warning,
                evidence_class: EvidenceClass::Factual,
                finding: format!(
                    "PID {pid} ({user}) idle in transaction for {duration}. \
                     Last query: {last_query}"
                ),
                proposed_action: format!("SELECT pg_terminate_backend({pid})"),
                expected_outcome: format!(
                    "Terminate idle-in-transaction session PID {pid}, \
                     releasing held locks and connection slot"
                ),
                risk: "The session's uncommitted transaction will be rolled back. \
                       The application must reconnect."
                    .to_owned(),
                created_at: std::time::SystemTime::now(),
            });
        }
    }
}

async fn propose_timeout_gucs(client: &Client, proposals: &mut Vec<ActionProposal>) {
    // Check idle_in_transaction_session_timeout.
    if let Ok(messages) = client.simple_query(IDLE_TIMEOUT_CHECK_SQL).await {
        for msg in &messages {
            if let tokio_postgres::SimpleQueryMessage::Row(row) = msg {
                let val = row.get(0).unwrap_or("0");
                if val == "0" {
                    proposals.push(ActionProposal {
                        feature: FeatureArea::ConfigTuning,
                        severity: Severity::Warning,
                        evidence_class: EvidenceClass::Heuristic,
                        finding: "idle_in_transaction_session_timeout is disabled (0). \
                                  Idle-in-transaction sessions can hold locks indefinitely."
                            .to_owned(),
                        proposed_action:
                            "ALTER SYSTEM SET idle_in_transaction_session_timeout = '5min'"
                                .to_owned(),
                        expected_outcome:
                            "Sessions idle in transaction for >5 minutes will be terminated automatically"
                                .to_owned(),
                        risk: "Applications with long-running idle transactions will be disconnected. \
                               They must handle reconnection."
                            .to_owned(),
                        created_at: std::time::SystemTime::now(),
                    });
                }
            }
        }
    }

    // Check statement_timeout.
    if let Ok(messages) = client.simple_query(STATEMENT_TIMEOUT_CHECK_SQL).await {
        for msg in &messages {
            if let tokio_postgres::SimpleQueryMessage::Row(row) = msg {
                let val = row.get(0).unwrap_or("0");
                if val == "0" {
                    proposals.push(ActionProposal {
                        feature: FeatureArea::ConfigTuning,
                        severity: Severity::Info,
                        evidence_class: EvidenceClass::Heuristic,
                        finding: "statement_timeout is disabled (0). Queries can run indefinitely."
                            .to_owned(),
                        proposed_action: "ALTER SYSTEM SET statement_timeout = '30s'".to_owned(),
                        expected_outcome:
                            "Queries exceeding 30 seconds will be cancelled automatically"
                                .to_owned(),
                        risk: "Long-running analytical queries or migrations may be terminated. \
                               Consider setting per-session timeouts instead."
                            .to_owned(),
                        created_at: std::time::SystemTime::now(),
                    });
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Supervised approval flow
// ---------------------------------------------------------------------------

/// Present proposals to the user and execute approved ones.
///
/// In Supervised mode, each proposal is:
/// 1. Validated by the Auditor
/// 2. Displayed to the user with risk assessment
/// 3. Executed only after explicit user confirmation
///
/// Returns the number of actions executed.
pub async fn run_supervised_flow(
    client: &Client,
    proposals: &[ActionProposal],
    audit_log: &mut AuditLog,
) -> usize {
    if proposals.is_empty() {
        return 0;
    }

    let auditor = Auditor;
    let mut actor = Actor::new();

    // Try to learn our own PID so we don't self-cancel.
    if let Ok(messages) = client.simple_query("SELECT pg_backend_pid()").await {
        for msg in &messages {
            if let tokio_postgres::SimpleQueryMessage::Row(row) = msg {
                if let Some(pid) = row.get(0).and_then(|s| s.parse::<i32>().ok()) {
                    actor.set_own_pid(pid);
                }
            }
        }
    }

    eprintln!("\n--- Proposed actions ({} total) ---\n", proposals.len());

    let mut executed = 0;
    for (i, proposal) in proposals.iter().enumerate() {
        if present_and_execute(client, proposal, i + 1, &auditor, &actor, audit_log).await {
            executed += 1;
        }
    }

    if executed > 0 {
        eprintln!("--- {executed} action(s) executed ---\n");
    } else {
        eprintln!("--- No actions executed ---\n");
    }

    executed
}

/// Execute proposals automatically in Auto mode.
///
/// Unlike Supervised mode, no user confirmation is required.
/// Actions are validated by the Auditor, checked against Auto constraints,
/// and the circuit breaker is consulted before execution.
///
/// Returns the number of actions executed.
#[allow(clippy::too_many_lines)]
pub async fn run_auto_flow(
    client: &Client,
    proposals: &[ActionProposal],
    audit_log: &mut AuditLog,
    circuit_breaker: &mut crate::governance::CircuitBreaker,
    veto_tracker: &mut crate::governance::VetoTracker,
) -> usize {
    if proposals.is_empty() {
        return 0;
    }

    let auditor = Auditor;
    let mut actor = Actor::new();

    // Learn our PID.
    if let Ok(messages) = client.simple_query("SELECT pg_backend_pid()").await {
        for msg in &messages {
            if let tokio_postgres::SimpleQueryMessage::Row(row) = msg {
                if let Some(pid) = row.get(0).and_then(|s| s.parse::<i32>().ok()) {
                    actor.set_own_pid(pid);
                }
            }
        }
    }

    let mut executed = 0;
    for proposal in proposals {
        // Check Auto action constraints.
        if !crate::governance::is_auto_permitted(proposal.feature, &proposal.proposed_action) {
            crate::logging::info(
                "auto",
                &format!(
                    "Skipping (not Auto-permitted): {}",
                    proposal.proposed_action
                ),
            );
            log_action(
                audit_log,
                proposal,
                AutonomyLevel::Auto,
                ActionOutcome::Skipped,
                None,
            );
            continue;
        }

        // Check veto tracker — previously vetoed actions skip Auto.
        if veto_tracker.is_vetoed(proposal.feature, &proposal.proposed_action) {
            crate::logging::info(
                "auto",
                &format!("Skipping (vetoed): {}", proposal.proposed_action),
            );
            log_action(
                audit_log,
                proposal,
                AutonomyLevel::Auto,
                ActionOutcome::Vetoed {
                    reason: "Previously vetoed by Auditor".to_owned(),
                },
                None,
            );
            continue;
        }

        // Auditor review (rule-based).
        let decision = auditor.review(proposal, AutonomyLevel::Auto);
        if let AuditDecision::Rejected { reason } = decision {
            crate::logging::info("auto", &format!("Auditor rejected: {reason}"));
            veto_tracker.record_veto(proposal.feature, &proposal.proposed_action);
            log_action(
                audit_log,
                proposal,
                AutonomyLevel::Auto,
                ActionOutcome::Vetoed { reason },
                None,
            );
            continue;
        }

        // Parse and validate.
        let Some(action_request) = parse_proposal_to_request(proposal) else {
            crate::logging::warn("auto", "Could not parse proposal into ActionRequest");
            log_action(
                audit_log,
                proposal,
                AutonomyLevel::Auto,
                ActionOutcome::Failure {
                    error: "Could not parse proposal into ActionRequest".to_owned(),
                },
                None,
            );
            continue;
        };
        if let Err(e) = actor.validate(&action_request) {
            crate::logging::warn("auto", &format!("Validation failed: {e}"));
            log_action(
                audit_log,
                proposal,
                AutonomyLevel::Auto,
                ActionOutcome::Failure {
                    error: format!("Validation failed: {e}"),
                },
                None,
            );
            continue;
        }

        // Execute.
        let outcome = actor.execute(client, &action_request).await;
        let success = matches!(outcome, ActionOutcome::Success { .. });

        // Capture verification result before consuming outcome.
        let verified_result: Option<bool> = if success {
            if let ActionOutcome::Success { ref detail } = outcome {
                crate::logging::info("auto", &format!("Executed: {detail}"));
            }
            // Post-action verification.
            let vr = crate::verification::verify_action(client, &action_request.action_type).await;
            crate::logging::info("auto", &format!("Verification: {vr}"));

            let verified = vr.is_confirmed();
            // Record in circuit breaker (verification result determines success).
            let tripped = circuit_breaker.record(proposal.feature, verified);
            if tripped {
                crate::logging::warn(
                    "auto",
                    &format!(
                        "Circuit breaker tripped for {} — downgrading to Supervised",
                        proposal.feature.label()
                    ),
                );
            }
            Some(verified)
        } else {
            if let ActionOutcome::Failure { ref error } = outcome {
                crate::logging::warn("auto", &format!("Failed: {error}"));
            }
            circuit_breaker.record(proposal.feature, false);
            None
        };

        let seq = log_action(audit_log, proposal, AutonomyLevel::Auto, outcome, None);
        if let Some(verified) = verified_result {
            audit_log.set_verification(seq, verified);
        }
        if success {
            executed += 1;
        }
    }

    if executed > 0 {
        crate::logging::info("auto", &format!("{executed} Auto action(s) executed"));
    }

    executed
}

/// Log an action to the audit log.
///
/// Returns the sequence number of the new entry, so callers can
/// update it later (e.g., with a verification result).
fn log_action(
    audit_log: &mut AuditLog,
    proposal: &ActionProposal,
    autonomy: AutonomyLevel,
    outcome: ActionOutcome,
    note: Option<String>,
) -> u64 {
    audit_log.record(
        proposal.feature,
        autonomy,
        proposal.proposed_action.clone(),
        proposal.finding.clone(),
        outcome,
        note,
    )
}

/// Present a single proposal, prompt the user, and execute if approved.
///
/// Returns `true` if the action was executed successfully.
#[allow(clippy::too_many_lines)]
async fn present_and_execute(
    client: &Client,
    proposal: &ActionProposal,
    num: usize,
    auditor: &Auditor,
    actor: &Actor,
    audit_log: &mut AuditLog,
) -> bool {
    let decision = auditor.review(proposal, AutonomyLevel::Supervised);

    let note = match decision {
        AuditDecision::Rejected { reason } => {
            eprintln!("  [{num}] REJECTED by Auditor: {reason}");
            log_action(
                audit_log,
                proposal,
                AutonomyLevel::Supervised,
                ActionOutcome::Vetoed { reason },
                None,
            );
            return false;
        }
        AuditDecision::Approved { note } => note,
    };

    // Display proposal for user review.
    let severity_icon = match proposal.severity {
        Severity::Critical => "!!",
        Severity::Warning => "! ",
        Severity::Info => "  ",
    };
    eprintln!("  [{num}] {severity_icon} {}", proposal.finding);
    eprintln!("      Action: {}", proposal.proposed_action);
    eprintln!("      Expected: {}", proposal.expected_outcome);
    eprintln!("      Risk: {}", proposal.risk);
    if let Some(ref n) = note {
        eprintln!("      Auditor note: {n}");
    }

    // Prompt for user confirmation.
    eprint!("      Execute? [y/N] ");
    let mut input = String::new();
    if std::io::stdin().read_line(&mut input).is_err() {
        eprintln!("      (could not read input, skipping)");
        log_action(
            audit_log,
            proposal,
            AutonomyLevel::Supervised,
            ActionOutcome::Skipped,
            None,
        );
        return false;
    }
    if !matches!(input.trim(), "y" | "Y" | "yes" | "Yes") {
        eprintln!("      Skipped.\n");
        log_action(
            audit_log,
            proposal,
            AutonomyLevel::Supervised,
            ActionOutcome::Skipped,
            None,
        );
        return false;
    }

    // Convert proposal to ActionRequest.
    let Some(action_request) = parse_proposal_to_request(proposal) else {
        eprintln!("      Could not parse action. Skipping.\n");
        log_action(
            audit_log,
            proposal,
            AutonomyLevel::Supervised,
            ActionOutcome::Failure {
                error: "Could not parse proposal into ActionRequest".to_owned(),
            },
            None,
        );
        return false;
    };

    // Validate through Actor.
    if let Err(e) = actor.validate(&action_request) {
        eprintln!("      Validation failed: {e}\n");
        log_action(
            audit_log,
            proposal,
            AutonomyLevel::Supervised,
            ActionOutcome::Vetoed {
                reason: e.to_string(),
            },
            None,
        );
        return false;
    }

    // Execute.
    let outcome = actor.execute(client, &action_request).await;
    let success = matches!(outcome, ActionOutcome::Success { .. });

    // Post-action verification (on success only).
    let (verified_note, verified_result) = if success {
        if let ActionOutcome::Success { detail } = &outcome {
            eprintln!("      Done: {detail}");
        }
        let vr = crate::verification::verify_action(client, &action_request.action_type).await;
        eprintln!("      Verify: {vr}\n");

        let confirmed = vr.is_confirmed();
        // Append verification result to auditor note.
        let vr_str = format!(" | Verification: {vr}");
        let combined_note = Some(match note {
            Some(n) => format!("{n}{vr_str}"),
            None => vr_str,
        });
        (combined_note, Some(confirmed))
    } else {
        match &outcome {
            ActionOutcome::Failure { error } => eprintln!("      Failed: {error}\n"),
            other => eprintln!("      {other:?}\n"),
        }
        (note, None)
    };
    let seq = log_action(
        audit_log,
        proposal,
        AutonomyLevel::Supervised,
        outcome,
        verified_note,
    );
    if let Some(verified) = verified_result {
        audit_log.set_verification(seq, verified);
    }
    success
}

// ---------------------------------------------------------------------------
// Proposal parsing
// ---------------------------------------------------------------------------

/// Convert an `ActionProposal` into a structured `ActionRequest`.
///
/// Parses the `proposed_action` SQL string to determine the action type.
/// Returns `None` if the action string doesn't match a known pattern.
fn parse_proposal_to_request(proposal: &ActionProposal) -> Option<ActionRequest> {
    let sql = proposal.proposed_action.trim();

    // SELECT pg_cancel_backend(PID)
    if let Some(rest) = sql
        .strip_prefix("SELECT pg_cancel_backend(")
        .or_else(|| sql.strip_prefix("select pg_cancel_backend("))
    {
        let pid_str = rest.trim_end_matches(')');
        let pid: i32 = pid_str.parse().ok()?;
        return Some(ActionRequest {
            feature: proposal.feature,
            action_type: ActionType::CancelQuery { pid },
            justification: proposal.finding.clone(),
        });
    }

    // SELECT pg_terminate_backend(PID)
    if let Some(rest) = sql
        .strip_prefix("SELECT pg_terminate_backend(")
        .or_else(|| sql.strip_prefix("select pg_terminate_backend("))
    {
        let pid_str = rest.trim_end_matches(')');
        let pid: i32 = pid_str.parse().ok()?;
        return Some(ActionRequest {
            feature: proposal.feature,
            action_type: ActionType::TerminateBackend { pid },
            justification: proposal.finding.clone(),
        });
    }

    // ALTER SYSTEM SET name = 'value'
    if let Some(rest) = sql
        .strip_prefix("ALTER SYSTEM SET ")
        .or_else(|| sql.strip_prefix("alter system set "))
    {
        let (name, value) = parse_set_assignment(rest)?;
        return Some(ActionRequest {
            feature: proposal.feature,
            action_type: ActionType::AlterSystemSet { name, value },
            justification: proposal.finding.clone(),
        });
    }

    // SET name = 'value'
    if let Some(rest) = sql
        .strip_prefix("SET ")
        .or_else(|| sql.strip_prefix("set "))
    {
        let (name, value) = parse_set_assignment(rest)?;
        return Some(ActionRequest {
            feature: proposal.feature,
            action_type: ActionType::SetSessionGuc { name, value },
            justification: proposal.finding.clone(),
        });
    }

    // ALTER SYSTEM RESET name
    if let Some(rest) = sql
        .strip_prefix("ALTER SYSTEM RESET ")
        .or_else(|| sql.strip_prefix("alter system reset "))
    {
        return Some(ActionRequest {
            feature: proposal.feature,
            action_type: ActionType::AlterSystemReset {
                name: rest.trim().to_owned(),
            },
            justification: proposal.finding.clone(),
        });
    }

    None
}

/// Parse `name = 'value'` from a SET/ALTER SYSTEM SET statement.
fn parse_set_assignment(s: &str) -> Option<(String, String)> {
    let (name, rest) = s.split_once('=')?;
    let name = name.trim().to_owned();
    let value = rest
        .trim()
        .trim_start_matches('\'')
        .trim_end_matches('\'')
        .to_owned();
    Some((name, value))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Truncate a string to `max_len` characters, appending "..." if truncated.
fn truncate(s: &str, max_len: usize) -> &str {
    if s.len() <= max_len {
        s
    } else {
        &s[..max_len]
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_cancel_backend() {
        let proposal = ActionProposal {
            feature: FeatureArea::Rca,
            severity: Severity::Critical,
            evidence_class: EvidenceClass::Factual,
            finding: "PID 1234 is blocking".to_owned(),
            proposed_action: "SELECT pg_cancel_backend(1234)".to_owned(),
            expected_outcome: String::new(),
            risk: String::new(),
            created_at: std::time::SystemTime::now(),
        };
        let req = parse_proposal_to_request(&proposal).unwrap();
        assert!(matches!(
            req.action_type,
            ActionType::CancelQuery { pid: 1234 }
        ));
        assert_eq!(req.feature, FeatureArea::Rca);
    }

    #[test]
    fn parse_terminate_backend() {
        let proposal = ActionProposal {
            feature: FeatureArea::Rca,
            severity: Severity::Warning,
            evidence_class: EvidenceClass::Heuristic,
            finding: "idle session".to_owned(),
            proposed_action: "SELECT pg_terminate_backend(5678)".to_owned(),
            expected_outcome: String::new(),
            risk: String::new(),
            created_at: std::time::SystemTime::now(),
        };
        let req = parse_proposal_to_request(&proposal).unwrap();
        assert!(matches!(
            req.action_type,
            ActionType::TerminateBackend { pid: 5678 }
        ));
    }

    #[test]
    fn parse_alter_system_set() {
        let proposal = ActionProposal {
            feature: FeatureArea::ConfigTuning,
            severity: Severity::Warning,
            evidence_class: EvidenceClass::Heuristic,
            finding: "timeout disabled".to_owned(),
            proposed_action: "ALTER SYSTEM SET idle_in_transaction_session_timeout = '5min'"
                .to_owned(),
            expected_outcome: String::new(),
            risk: String::new(),
            created_at: std::time::SystemTime::now(),
        };
        let req = parse_proposal_to_request(&proposal).unwrap();
        match req.action_type {
            ActionType::AlterSystemSet {
                ref name,
                ref value,
            } => {
                assert_eq!(name, "idle_in_transaction_session_timeout");
                assert_eq!(value, "5min");
            }
            _ => panic!("Expected AlterSystemSet"),
        }
    }

    #[test]
    fn parse_session_set() {
        let proposal = ActionProposal {
            feature: FeatureArea::ConfigTuning,
            severity: Severity::Info,
            evidence_class: EvidenceClass::Heuristic,
            finding: "tuning".to_owned(),
            proposed_action: "SET statement_timeout = '30s'".to_owned(),
            expected_outcome: String::new(),
            risk: String::new(),
            created_at: std::time::SystemTime::now(),
        };
        let req = parse_proposal_to_request(&proposal).unwrap();
        match req.action_type {
            ActionType::SetSessionGuc {
                ref name,
                ref value,
            } => {
                assert_eq!(name, "statement_timeout");
                assert_eq!(value, "30s");
            }
            _ => panic!("Expected SetSessionGuc"),
        }
    }

    #[test]
    fn parse_alter_system_reset() {
        let proposal = ActionProposal {
            feature: FeatureArea::ConfigTuning,
            severity: Severity::Info,
            evidence_class: EvidenceClass::Heuristic,
            finding: "reset".to_owned(),
            proposed_action: "ALTER SYSTEM RESET work_mem".to_owned(),
            expected_outcome: String::new(),
            risk: String::new(),
            created_at: std::time::SystemTime::now(),
        };
        let req = parse_proposal_to_request(&proposal).unwrap();
        match req.action_type {
            ActionType::AlterSystemReset { ref name } => {
                assert_eq!(name, "work_mem");
            }
            _ => panic!("Expected AlterSystemReset"),
        }
    }

    #[test]
    fn parse_unknown_returns_none() {
        let proposal = ActionProposal {
            feature: FeatureArea::Rca,
            severity: Severity::Info,
            evidence_class: EvidenceClass::Heuristic,
            finding: "unknown".to_owned(),
            proposed_action: "CREATE INDEX something".to_owned(),
            expected_outcome: String::new(),
            risk: String::new(),
            created_at: std::time::SystemTime::now(),
        };
        assert!(parse_proposal_to_request(&proposal).is_none());
    }

    #[test]
    fn truncate_short_string() {
        assert_eq!(truncate("hello", 10), "hello");
    }

    #[test]
    fn truncate_long_string() {
        assert_eq!(truncate("hello world", 5), "hello");
    }

    #[test]
    fn parse_set_assignment_works() {
        let (name, value) = parse_set_assignment("work_mem = '256MB'").unwrap();
        assert_eq!(name, "work_mem");
        assert_eq!(value, "256MB");
    }

    #[test]
    fn parse_set_assignment_no_equals_returns_none() {
        assert!(parse_set_assignment("work_mem 256MB").is_none());
    }
}
