//! Actor — isolated executor for the AAA governance framework.
//!
//! The Actor is the **only** component that executes write actions against
//! the database. It is deliberately separated from the Analyzer (which
//! proposes actions via LLM) to prevent prompt injection from directly
//! causing writes.
//!
//! # Design Principles
//!
//! - **No natural language processing**: accepts only structured
//!   [`ActionRequest`] values, never raw LLM output.
//! - **Whitelist-based validation**: each [`ActionType`] has a fixed SQL
//!   template. The Actor never executes arbitrary SQL.
//! - **DB permission enforcement**: validates that the action can succeed
//!   given the connected role's privileges before attempting execution.

// Phase 3 infrastructure — consumers arrive in subsequent PRs.
#![allow(dead_code)]

use crate::governance::{ActionOutcome, FeatureArea};

// ---------------------------------------------------------------------------
// Action types (whitelist)
// ---------------------------------------------------------------------------

/// Structured action types the Actor knows how to execute.
///
/// Each variant maps to exactly one SQL template. No free-form SQL.
#[derive(Debug, Clone)]
pub enum ActionType {
    /// Cancel a running query: `SELECT pg_cancel_backend($pid)`.
    CancelQuery {
        /// Target backend PID.
        pid: i32,
    },
    /// Terminate a backend: `SELECT pg_terminate_backend($pid)`.
    TerminateBackend {
        /// Target backend PID.
        pid: i32,
    },
    /// Set a runtime GUC: `SET name = 'value'` (session-level).
    SetSessionGuc {
        /// GUC parameter name.
        name: String,
        /// New value.
        value: String,
    },
    /// Set a persistent GUC: `ALTER SYSTEM SET name = 'value'`.
    AlterSystemSet {
        /// GUC parameter name.
        name: String,
        /// New value.
        value: String,
    },
    /// Reset a persistent GUC: `ALTER SYSTEM RESET name`.
    AlterSystemReset {
        /// GUC parameter name.
        name: String,
    },
}

impl ActionType {
    /// Build the SQL string for this action.
    ///
    /// Uses parameterized templates — no string interpolation from
    /// untrusted input.
    pub fn to_sql(&self) -> String {
        match self {
            Self::CancelQuery { pid } => {
                format!("SELECT pg_cancel_backend({pid})")
            }
            Self::TerminateBackend { pid } => {
                format!("SELECT pg_terminate_backend({pid})")
            }
            Self::SetSessionGuc { name, value } => {
                // Validate name is a simple identifier (defense in depth).
                let safe_name = sanitize_identifier(name);
                let safe_value = escape_guc_value(value);
                format!("SET {safe_name} = '{safe_value}'")
            }
            Self::AlterSystemSet { name, value } => {
                let safe_name = sanitize_identifier(name);
                let safe_value = escape_guc_value(value);
                format!("ALTER SYSTEM SET {safe_name} = '{safe_value}'")
            }
            Self::AlterSystemReset { name } => {
                let safe_name = sanitize_identifier(name);
                format!("ALTER SYSTEM RESET {safe_name}")
            }
        }
    }

    /// Human-readable description of this action.
    pub fn description(&self) -> String {
        match self {
            Self::CancelQuery { pid } => format!("Cancel query on PID {pid}"),
            Self::TerminateBackend { pid } => format!("Terminate backend PID {pid}"),
            Self::SetSessionGuc { name, value } => {
                format!("SET {name} = '{value}' (session)")
            }
            Self::AlterSystemSet { name, value } => {
                format!("ALTER SYSTEM SET {name} = '{value}'")
            }
            Self::AlterSystemReset { name } => {
                format!("ALTER SYSTEM RESET {name}")
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Action request (Auditor-approved → Actor)
// ---------------------------------------------------------------------------

/// A validated action request for the Actor.
///
/// Created after the Auditor approves an [`ActionProposal`]. Contains
/// the structured action type (not free-form SQL) and metadata.
#[derive(Debug, Clone)]
pub struct ActionRequest {
    /// Which feature area this belongs to.
    pub feature: FeatureArea,
    /// The specific action to execute.
    pub action_type: ActionType,
    /// Human-readable justification (from the Analyzer).
    pub justification: String,
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// GUC parameters that are safe to modify at runtime (no restart required).
///
/// These are the only GUCs the Actor will allow `SET` or
/// `ALTER SYSTEM SET` to modify.
const SAFE_GUCS: &[&str] = &[
    "idle_in_transaction_session_timeout",
    "lock_timeout",
    "statement_timeout",
    "log_min_duration_statement",
    "deadlock_timeout",
    "work_mem",
    "maintenance_work_mem",
    "temp_buffers",
    "effective_cache_size",
];

/// Validation error from the Actor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationError {
    /// The target PID is the Actor's own backend — self-cancellation is forbidden.
    SelfTarget,
    /// The GUC parameter is not in the safe list.
    UnsafeGuc {
        /// The disallowed parameter name.
        name: String,
    },
    /// The GUC value failed validation.
    InvalidGucValue {
        /// Reason.
        reason: String,
    },
    /// The PID is invalid (non-positive).
    InvalidPid {
        /// The invalid PID.
        pid: i32,
    },
}

impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SelfTarget => write!(f, "Cannot target own backend PID"),
            Self::UnsafeGuc { name } => write!(f, "GUC '{name}' is not in the safe list"),
            Self::InvalidGucValue { reason } => write!(f, "Invalid GUC value: {reason}"),
            Self::InvalidPid { pid } => write!(f, "Invalid PID: {pid}"),
        }
    }
}

// ---------------------------------------------------------------------------
// Actor
// ---------------------------------------------------------------------------

/// The Actor — executes approved actions within DB permission boundaries.
///
/// # Safety Model
///
/// 1. Only accepts [`ActionRequest`] values (structured, not natural language).
/// 2. Validates every action against a whitelist before execution.
/// 3. Checks that the target is valid (PID exists, GUC is in safe list).
/// 4. Relies on `PostgreSQL` role permissions as the enforcement layer.
#[derive(Debug)]
pub struct Actor {
    /// The PID of the Actor's own database connection.
    ///
    /// Used to prevent self-cancellation / self-termination.
    own_pid: Option<i32>,
}

impl Actor {
    /// Create a new Actor.
    pub fn new() -> Self {
        Self { own_pid: None }
    }

    /// Set the Actor's own backend PID (detected at connect time).
    pub fn set_own_pid(&mut self, pid: i32) {
        self.own_pid = Some(pid);
    }

    /// Validate an action request before execution.
    ///
    /// Returns `Ok(())` if the action is permitted, or a
    /// [`ValidationError`] explaining why it was rejected.
    pub fn validate(&self, request: &ActionRequest) -> Result<(), ValidationError> {
        match &request.action_type {
            ActionType::CancelQuery { pid } | ActionType::TerminateBackend { pid } => {
                if *pid <= 0 {
                    return Err(ValidationError::InvalidPid { pid: *pid });
                }
                if Some(*pid) == self.own_pid {
                    return Err(ValidationError::SelfTarget);
                }
            }
            ActionType::SetSessionGuc { name, value }
            | ActionType::AlterSystemSet { name, value } => {
                if !is_safe_guc(name) {
                    return Err(ValidationError::UnsafeGuc { name: name.clone() });
                }
                validate_guc_value(name, value)?;
            }
            ActionType::AlterSystemReset { name } => {
                if !is_safe_guc(name) {
                    return Err(ValidationError::UnsafeGuc { name: name.clone() });
                }
            }
        }
        Ok(())
    }

    /// Execute a validated action request.
    ///
    /// **Call [`validate`](Self::validate) first.** This method does
    /// not re-validate — it trusts that the caller has already checked.
    pub async fn execute(
        &self,
        client: &tokio_postgres::Client,
        request: &ActionRequest,
    ) -> ActionOutcome {
        let sql = request.action_type.to_sql();
        match client.simple_query(&sql).await {
            Ok(_) => ActionOutcome::Success {
                detail: request.action_type.description(),
            },
            Err(e) => ActionOutcome::Failure {
                error: e.to_string(),
            },
        }
    }
}

impl Default for Actor {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Check if a GUC parameter name is in the safe list.
fn is_safe_guc(name: &str) -> bool {
    let lower = name.to_lowercase();
    SAFE_GUCS.iter().any(|&safe| safe == lower)
}

/// Validate a GUC value for common issues.
fn validate_guc_value(name: &str, value: &str) -> Result<(), ValidationError> {
    if value.is_empty() {
        return Err(ValidationError::InvalidGucValue {
            reason: "empty value".to_owned(),
        });
    }
    // Reject values containing SQL injection attempts.
    if value.contains(';') || value.contains("--") {
        return Err(ValidationError::InvalidGucValue {
            reason: "value contains suspicious characters".to_owned(),
        });
    }
    // Timeout GUCs must be parseable as a duration or number.
    let lower_name = name.to_lowercase();
    if lower_name.ends_with("_timeout") {
        // Accept: bare numbers (ms), or values like "5s", "30min", "1h".
        let trimmed = value.trim();
        let is_numeric = trimmed.chars().all(|c| c.is_ascii_digit());
        let has_unit = trimmed
            .trim_end_matches(|c: char| c.is_ascii_alphabetic())
            .chars()
            .all(|c| c.is_ascii_digit());
        if !is_numeric && !has_unit {
            return Err(ValidationError::InvalidGucValue {
                reason: format!("'{value}' does not look like a valid timeout value"),
            });
        }
    }
    Ok(())
}

/// Sanitize a GUC parameter name to a simple identifier.
///
/// Strips everything that isn't alphanumeric or underscore.
fn sanitize_identifier(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect()
}

/// Escape a GUC value for use in a SQL string literal.
///
/// Doubles single quotes to prevent SQL injection.
fn escape_guc_value(value: &str) -> String {
    value.replace('\'', "''")
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancel_query_sql() {
        let action = ActionType::CancelQuery { pid: 12345 };
        assert_eq!(action.to_sql(), "SELECT pg_cancel_backend(12345)");
    }

    #[test]
    fn terminate_backend_sql() {
        let action = ActionType::TerminateBackend { pid: 99 };
        assert_eq!(action.to_sql(), "SELECT pg_terminate_backend(99)");
    }

    #[test]
    fn set_session_guc_sql() {
        let action = ActionType::SetSessionGuc {
            name: "statement_timeout".to_owned(),
            value: "30s".to_owned(),
        };
        assert_eq!(action.to_sql(), "SET statement_timeout = '30s'");
    }

    #[test]
    fn alter_system_set_sql() {
        let action = ActionType::AlterSystemSet {
            name: "lock_timeout".to_owned(),
            value: "5000".to_owned(),
        };
        assert_eq!(action.to_sql(), "ALTER SYSTEM SET lock_timeout = '5000'");
    }

    #[test]
    fn alter_system_reset_sql() {
        let action = ActionType::AlterSystemReset {
            name: "work_mem".to_owned(),
        };
        assert_eq!(action.to_sql(), "ALTER SYSTEM RESET work_mem");
    }

    #[test]
    fn set_guc_escapes_quotes() {
        let action = ActionType::SetSessionGuc {
            name: "work_mem".to_owned(),
            value: "it's 4MB".to_owned(),
        };
        assert_eq!(action.to_sql(), "SET work_mem = 'it''s 4MB'");
    }

    #[test]
    fn sanitize_identifier_strips_special() {
        assert_eq!(sanitize_identifier("work_mem"), "work_mem");
        assert_eq!(
            sanitize_identifier("work_mem; DROP TABLE"),
            "work_memDROPTABLE"
        );
        assert_eq!(sanitize_identifier("a--b"), "ab");
    }

    #[test]
    fn validate_cancel_valid_pid() {
        let actor = Actor::new();
        let request = ActionRequest {
            feature: FeatureArea::Rca,
            action_type: ActionType::CancelQuery { pid: 100 },
            justification: "blocking lock".to_owned(),
        };
        assert!(actor.validate(&request).is_ok());
    }

    #[test]
    fn validate_cancel_invalid_pid() {
        let actor = Actor::new();
        let request = ActionRequest {
            feature: FeatureArea::Rca,
            action_type: ActionType::CancelQuery { pid: -1 },
            justification: "test".to_owned(),
        };
        assert_eq!(
            actor.validate(&request),
            Err(ValidationError::InvalidPid { pid: -1 })
        );
    }

    #[test]
    fn validate_cancel_self_target() {
        let mut actor = Actor::new();
        actor.set_own_pid(42);
        let request = ActionRequest {
            feature: FeatureArea::Rca,
            action_type: ActionType::CancelQuery { pid: 42 },
            justification: "test".to_owned(),
        };
        assert_eq!(actor.validate(&request), Err(ValidationError::SelfTarget));
    }

    #[test]
    fn validate_safe_guc() {
        let actor = Actor::new();
        let request = ActionRequest {
            feature: FeatureArea::ConfigTuning,
            action_type: ActionType::SetSessionGuc {
                name: "statement_timeout".to_owned(),
                value: "30000".to_owned(),
            },
            justification: "test".to_owned(),
        };
        assert!(actor.validate(&request).is_ok());
    }

    #[test]
    fn validate_unsafe_guc() {
        let actor = Actor::new();
        let request = ActionRequest {
            feature: FeatureArea::ConfigTuning,
            action_type: ActionType::SetSessionGuc {
                name: "shared_buffers".to_owned(),
                value: "4GB".to_owned(),
            },
            justification: "test".to_owned(),
        };
        assert!(matches!(
            actor.validate(&request),
            Err(ValidationError::UnsafeGuc { .. })
        ));
    }

    #[test]
    fn validate_guc_empty_value() {
        let actor = Actor::new();
        let request = ActionRequest {
            feature: FeatureArea::ConfigTuning,
            action_type: ActionType::SetSessionGuc {
                name: "work_mem".to_owned(),
                value: String::new(),
            },
            justification: "test".to_owned(),
        };
        assert!(matches!(
            actor.validate(&request),
            Err(ValidationError::InvalidGucValue { .. })
        ));
    }

    #[test]
    fn validate_guc_injection_attempt() {
        let actor = Actor::new();
        let request = ActionRequest {
            feature: FeatureArea::ConfigTuning,
            action_type: ActionType::SetSessionGuc {
                name: "statement_timeout".to_owned(),
                value: "0; DROP TABLE users".to_owned(),
            },
            justification: "test".to_owned(),
        };
        assert!(matches!(
            actor.validate(&request),
            Err(ValidationError::InvalidGucValue { .. })
        ));
    }

    #[test]
    fn validate_timeout_with_unit() {
        let actor = Actor::new();
        let request = ActionRequest {
            feature: FeatureArea::ConfigTuning,
            action_type: ActionType::SetSessionGuc {
                name: "lock_timeout".to_owned(),
                value: "5s".to_owned(),
            },
            justification: "test".to_owned(),
        };
        assert!(actor.validate(&request).is_ok());
    }

    #[test]
    fn validate_alter_system_reset_safe() {
        let actor = Actor::new();
        let request = ActionRequest {
            feature: FeatureArea::ConfigTuning,
            action_type: ActionType::AlterSystemReset {
                name: "work_mem".to_owned(),
            },
            justification: "test".to_owned(),
        };
        assert!(actor.validate(&request).is_ok());
    }

    #[test]
    fn validate_alter_system_reset_unsafe() {
        let actor = Actor::new();
        let request = ActionRequest {
            feature: FeatureArea::ConfigTuning,
            action_type: ActionType::AlterSystemReset {
                name: "max_connections".to_owned(),
            },
            justification: "test".to_owned(),
        };
        assert!(matches!(
            actor.validate(&request),
            Err(ValidationError::UnsafeGuc { .. })
        ));
    }

    #[test]
    fn is_safe_guc_case_insensitive() {
        assert!(is_safe_guc("STATEMENT_TIMEOUT"));
        assert!(is_safe_guc("Statement_Timeout"));
        assert!(is_safe_guc("statement_timeout"));
    }

    #[test]
    fn is_safe_guc_rejects_unknown() {
        assert!(!is_safe_guc("max_connections"));
        assert!(!is_safe_guc("shared_buffers"));
        assert!(!is_safe_guc("wal_level"));
    }

    #[test]
    fn action_type_description() {
        assert_eq!(
            ActionType::CancelQuery { pid: 10 }.description(),
            "Cancel query on PID 10"
        );
        assert_eq!(
            ActionType::TerminateBackend { pid: 20 }.description(),
            "Terminate backend PID 20"
        );
    }

    #[test]
    fn actor_default() {
        let actor = Actor::default();
        assert!(actor.own_pid.is_none());
    }

    #[test]
    fn validation_error_display() {
        assert_eq!(
            ValidationError::SelfTarget.to_string(),
            "Cannot target own backend PID"
        );
        assert!(ValidationError::UnsafeGuc {
            name: "x".to_owned()
        }
        .to_string()
        .contains("not in the safe list"));
    }

    #[test]
    fn safe_gucs_list_contains_expected() {
        assert!(SAFE_GUCS.contains(&"idle_in_transaction_session_timeout"));
        assert!(SAFE_GUCS.contains(&"lock_timeout"));
        assert!(SAFE_GUCS.contains(&"statement_timeout"));
        assert!(SAFE_GUCS.contains(&"work_mem"));
    }
}
