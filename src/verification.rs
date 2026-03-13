//! Post-action verification for the AAA framework.
//!
//! After the Actor executes an action, the Auditor can verify whether
//! the action achieved its intended effect.  This module provides
//! verification queries for each action type.

use crate::actor::ActionType;
use tokio_postgres::Client;

// ---------------------------------------------------------------------------
// Verification results
// ---------------------------------------------------------------------------

/// Result of post-action verification.
#[derive(Debug, Clone)]
pub enum VerificationResult {
    /// Action achieved its intended effect.
    Confirmed { detail: String },
    /// Action did not achieve its intended effect.
    NotConfirmed { detail: String },
    /// Verification could not be performed (e.g., query error).
    Inconclusive { reason: String },
}

impl VerificationResult {
    /// Whether the action was confirmed successful.
    pub fn is_confirmed(&self) -> bool {
        matches!(self, Self::Confirmed { .. })
    }
}

impl std::fmt::Display for VerificationResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Confirmed { detail } => write!(f, "Verified: {detail}"),
            Self::NotConfirmed { detail } => write!(f, "NOT verified: {detail}"),
            Self::Inconclusive { reason } => write!(f, "Inconclusive: {reason}"),
        }
    }
}

// ---------------------------------------------------------------------------
// Verification queries
// ---------------------------------------------------------------------------

/// Verify that an action achieved its intended effect.
///
/// Should be called shortly after `Actor::execute()` returns success.
pub async fn verify_action(client: &Client, action: &ActionType) -> VerificationResult {
    match action {
        ActionType::CancelQuery { pid } => verify_cancel(client, *pid).await,
        ActionType::TerminateBackend { pid } => verify_terminate(client, *pid).await,
        ActionType::SetSessionGuc { name, value } => verify_session_guc(client, name, value).await,
        ActionType::AlterSystemSet { name, value } => {
            verify_alter_system(client, name, value).await
        }
        ActionType::AlterSystemReset { name } => verify_alter_system_reset(client, name).await,
    }
}

/// Verify that a cancelled query is no longer active.
async fn verify_cancel(client: &Client, pid: i32) -> VerificationResult {
    let sql = format!(
        "SELECT state FROM pg_stat_activity WHERE pid = {pid} AND backend_type = 'client backend'"
    );
    match client.simple_query(&sql).await {
        Ok(messages) => {
            for msg in &messages {
                if let tokio_postgres::SimpleQueryMessage::Row(row) = msg {
                    let state = row.get(0).unwrap_or("unknown");
                    if state == "active" {
                        return VerificationResult::NotConfirmed {
                            detail: format!("PID {pid} is still active after cancel"),
                        };
                    }
                    return VerificationResult::Confirmed {
                        detail: format!("PID {pid} state: {state}"),
                    };
                }
            }
            // No row means PID disconnected — cancel succeeded.
            VerificationResult::Confirmed {
                detail: format!("PID {pid} is no longer connected"),
            }
        }
        Err(e) => VerificationResult::Inconclusive {
            reason: e.to_string(),
        },
    }
}

/// Verify that a terminated backend is gone.
async fn verify_terminate(client: &Client, pid: i32) -> VerificationResult {
    let sql = format!(
        "SELECT 1 FROM pg_stat_activity WHERE pid = {pid} AND backend_type = 'client backend'"
    );
    match client.simple_query(&sql).await {
        Ok(messages) => {
            let has_row = messages
                .iter()
                .any(|m| matches!(m, tokio_postgres::SimpleQueryMessage::Row(_)));
            if has_row {
                VerificationResult::NotConfirmed {
                    detail: format!("PID {pid} is still present after terminate"),
                }
            } else {
                VerificationResult::Confirmed {
                    detail: format!("PID {pid} has been terminated"),
                }
            }
        }
        Err(e) => VerificationResult::Inconclusive {
            reason: e.to_string(),
        },
    }
}

/// Verify that a session GUC was set to the expected value.
async fn verify_session_guc(client: &Client, name: &str, expected: &str) -> VerificationResult {
    let sql = format!("SHOW {name}");
    match client.simple_query(&sql).await {
        Ok(messages) => {
            for msg in &messages {
                if let tokio_postgres::SimpleQueryMessage::Row(row) = msg {
                    let actual = row.get(0).unwrap_or("");
                    if actual == expected {
                        return VerificationResult::Confirmed {
                            detail: format!("{name} = {actual}"),
                        };
                    }
                    return VerificationResult::NotConfirmed {
                        detail: format!("{name} = {actual} (expected {expected})"),
                    };
                }
            }
            VerificationResult::Inconclusive {
                reason: "No result from SHOW".to_owned(),
            }
        }
        Err(e) => VerificationResult::Inconclusive {
            reason: e.to_string(),
        },
    }
}

/// Verify that ALTER SYSTEM SET was applied to `postgresql.auto.conf`.
///
/// Note: The change requires `pg_reload_conf()` to take effect in
/// running sessions. This only verifies the pending configuration.
async fn verify_alter_system(client: &Client, name: &str, expected: &str) -> VerificationResult {
    let sql = format!(
        "SELECT setting FROM pg_file_settings \
         WHERE name = '{name}' AND applied ORDER BY seqno DESC LIMIT 1"
    );
    match client.simple_query(&sql).await {
        Ok(messages) => {
            for msg in &messages {
                if let tokio_postgres::SimpleQueryMessage::Row(row) = msg {
                    let actual = row.get(0).unwrap_or("");
                    if actual == expected {
                        return VerificationResult::Confirmed {
                            detail: format!(
                                "{name} = {actual} in postgresql.auto.conf (reload needed)"
                            ),
                        };
                    }
                    return VerificationResult::NotConfirmed {
                        detail: format!("{name} = {actual} in auto.conf (expected {expected})"),
                    };
                }
            }
            // No row might mean the setting wasn't written.
            VerificationResult::Inconclusive {
                reason: format!("{name} not found in pg_file_settings"),
            }
        }
        Err(e) => VerificationResult::Inconclusive {
            reason: e.to_string(),
        },
    }
}

/// Verify that ALTER SYSTEM RESET removed the setting.
async fn verify_alter_system_reset(client: &Client, name: &str) -> VerificationResult {
    let sql = format!("SELECT 1 FROM pg_file_settings WHERE name = '{name}' AND applied");
    match client.simple_query(&sql).await {
        Ok(messages) => {
            let has_row = messages
                .iter()
                .any(|m| matches!(m, tokio_postgres::SimpleQueryMessage::Row(_)));
            if has_row {
                VerificationResult::NotConfirmed {
                    detail: format!("{name} still present in postgresql.auto.conf"),
                }
            } else {
                VerificationResult::Confirmed {
                    detail: format!("{name} removed from postgresql.auto.conf"),
                }
            }
        }
        Err(e) => VerificationResult::Inconclusive {
            reason: e.to_string(),
        },
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verification_result_display() {
        let confirmed = VerificationResult::Confirmed {
            detail: "PID gone".to_owned(),
        };
        assert!(confirmed.to_string().contains("Verified"));
        assert!(confirmed.is_confirmed());

        let not_confirmed = VerificationResult::NotConfirmed {
            detail: "still active".to_owned(),
        };
        assert!(not_confirmed.to_string().contains("NOT verified"));
        assert!(!not_confirmed.is_confirmed());

        let inconclusive = VerificationResult::Inconclusive {
            reason: "query failed".to_owned(),
        };
        assert!(inconclusive.to_string().contains("Inconclusive"));
        assert!(!inconclusive.is_confirmed());
    }
}
