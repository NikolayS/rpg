//! Database capability detection.
//!
//! Probes the connected database at startup to determine what extensions
//! and features are available. Results are stored in [`DbCapabilities`]
//! and used by the AI subsystem and governance framework.

// ---------------------------------------------------------------------------
// Capability state
// ---------------------------------------------------------------------------

/// Status of the `pg_ash` extension.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum PgAshStatus {
    /// Extension not detected (degraded mode: `pg_stat_activity` only).
    #[default]
    NotAvailable,
    /// Extension is installed and responding.
    Available {
        /// Version string from `ash.status()`, if provided.
        version: Option<String>,
    },
    /// Detection query failed (e.g. permissions, connection error).
    DetectionFailed {
        /// Error message from the probe.
        error: String,
    },
}

impl PgAshStatus {
    /// Whether `pg_ash` is available for use.
    #[allow(dead_code)]
    pub fn is_available(&self) -> bool {
        matches!(self, Self::Available { .. })
    }
}

/// Detected database capabilities.
///
/// Populated once at connection time and stored in [`ReplSettings`] for
/// use by the AI context builder, governance framework, and RCA subsystem.
#[derive(Debug, Clone, Default)]
pub struct DbCapabilities {
    /// `pg_ash` extension status.
    pub pg_ash: PgAshStatus,
    /// `PostgreSQL` server version string (e.g. `"16.2"`).
    #[allow(dead_code)]
    pub server_version: Option<String>,
}

// ---------------------------------------------------------------------------
// Detection
// ---------------------------------------------------------------------------

/// Detect database capabilities by probing the connected server.
///
/// This runs lightweight queries against the catalog and extensions.
/// Errors are captured in the returned struct, not propagated — callers
/// always get a valid [`DbCapabilities`].
pub async fn detect(client: &tokio_postgres::Client) -> DbCapabilities {
    let pg_ash = detect_pg_ash(client).await;
    let server_version = detect_server_version(client).await;

    DbCapabilities {
        pg_ash,
        server_version,
    }
}

/// Probe for `pg_ash` by querying `ash.status()`.
async fn detect_pg_ash(client: &tokio_postgres::Client) -> PgAshStatus {
    // First check if the ash schema exists at all (avoids noisy errors).
    let schema_check = client
        .simple_query("SELECT 1 FROM pg_catalog.pg_namespace WHERE nspname = 'ash'")
        .await;

    match schema_check {
        Ok(msgs) => {
            let has_row = msgs
                .iter()
                .any(|m| matches!(m, tokio_postgres::SimpleQueryMessage::Row(_)));
            if !has_row {
                return PgAshStatus::NotAvailable;
            }
        }
        Err(e) => {
            return PgAshStatus::DetectionFailed {
                error: e.to_string(),
            };
        }
    }

    // Schema exists — try calling ash.status() for version info.
    match client
        .simple_query("SELECT version FROM ash.status()")
        .await
    {
        Ok(msgs) => {
            let version = msgs.iter().find_map(|m| {
                if let tokio_postgres::SimpleQueryMessage::Row(row) = m {
                    row.get(0).map(String::from)
                } else {
                    None
                }
            });
            PgAshStatus::Available { version }
        }
        Err(_) => {
            // Schema exists but status() failed — extension is partially
            // installed or permissions issue. Still mark as available since
            // the schema is present.
            PgAshStatus::Available { version: None }
        }
    }
}

/// Read the `PostgreSQL` server version.
async fn detect_server_version(client: &tokio_postgres::Client) -> Option<String> {
    match client.simple_query("SHOW server_version").await {
        Ok(msgs) => msgs.iter().find_map(|m| {
            if let tokio_postgres::SimpleQueryMessage::Row(row) = m {
                row.get(0).map(String::from)
            } else {
                None
            }
        }),
        Err(_) => None,
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pg_ash_status_default_is_not_available() {
        assert_eq!(PgAshStatus::default(), PgAshStatus::NotAvailable);
    }

    #[test]
    fn pg_ash_available_is_available() {
        let status = PgAshStatus::Available {
            version: Some("1.0".to_owned()),
        };
        assert!(status.is_available());
    }

    #[test]
    fn pg_ash_not_available_is_not_available() {
        assert!(!PgAshStatus::NotAvailable.is_available());
    }

    #[test]
    fn pg_ash_detection_failed_is_not_available() {
        let status = PgAshStatus::DetectionFailed {
            error: "timeout".to_owned(),
        };
        assert!(!status.is_available());
    }

    #[test]
    fn db_capabilities_default() {
        let caps = DbCapabilities::default();
        assert!(!caps.pg_ash.is_available());
        assert!(caps.server_version.is_none());
    }

    #[test]
    fn pg_ash_available_without_version() {
        let status = PgAshStatus::Available { version: None };
        assert!(status.is_available());
    }
}
