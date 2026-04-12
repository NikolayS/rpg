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

/// Connection pooler type detected in front of the database.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum PoolerType {
    /// No pooler detected; direct connection to Postgres.
    #[default]
    None,
    /// `PgBouncer` pooler with the detected pool mode.
    PgBouncer {
        /// Pool mode reported by `SHOW pool_mode` (e.g. `"transaction"`).
        pool_mode: String,
    },
    /// Supavisor pooler (Supabase's connection pooler).
    Supavisor,
    /// `PgCat` pooler.
    PgCat,
}

impl PoolerType {
    /// Whether any pooler is in use.
    #[allow(dead_code)]
    pub fn is_pooled(&self) -> bool {
        !matches!(self, Self::None)
    }

    /// Return a warning string when transaction-mode pooling is active.
    ///
    /// Features such as prepared statements, advisory locks, and
    /// session-level settings break under transaction-mode pooling.
    /// Returns `None` when no relevant warning applies.
    pub fn pooler_warning(&self) -> Option<&str> {
        match self {
            Self::PgBouncer { pool_mode } if pool_mode == "transaction" => Some(
                "PgBouncer is running in transaction mode. \
                 Prepared statements, advisory locks, and session-level \
                 settings are not supported.",
            ),
            Self::Supavisor => Some(
                "Supavisor is active. Some session-level features may \
                 not be available depending on pool mode.",
            ),
            Self::PgCat => Some(
                "PgCat is active. Some session-level features may \
                 not be available depending on pool mode.",
            ),
            _ => None,
        }
    }
}

/// Managed Postgres provider detected from server GUCs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ManagedProvider {
    /// Not a recognised managed provider.
    #[default]
    None,
    /// Amazon RDS or Aurora.
    Rds,
    /// Google Cloud SQL.
    CloudSql,
    /// Supabase managed Postgres.
    Supabase,
    /// Neon serverless Postgres.
    Neon,
}

/// Detected database capabilities.
///
/// Populated once at connection time and stored in [`ReplSettings`] for
/// use by the AI context builder, governance framework, and RCA subsystem.
#[derive(Debug, Clone)]
pub struct DbCapabilities {
    /// `pg_ash` extension status.
    pub pg_ash: PgAshStatus,
    /// `PostgreSQL` server version string (e.g. `"16.2"`).
    pub server_version: Option<String>,
    /// Connection pooler detected in front of this database.
    pub pooler: PoolerType,
    /// Managed Postgres provider, if recognised.
    pub managed_provider: ManagedProvider,
    /// Value of `standard_conforming_strings` GUC.
    ///
    /// When `false`, backslash inside single-quoted string literals is an
    /// escape character (C-style escapes).  Default in modern PG is `true`.
    pub standard_conforming_strings: bool,
}

impl Default for DbCapabilities {
    fn default() -> Self {
        Self {
            pg_ash: PgAshStatus::default(),
            server_version: None,
            pooler: PoolerType::default(),
            managed_provider: ManagedProvider::default(),
            // PostgreSQL default is `on` since PG 9.1.
            standard_conforming_strings: true,
        }
    }
}

impl DbCapabilities {
    /// Parse the major version number from the server version string.
    ///
    /// Returns `None` if the version string is absent or unparseable.
    pub fn pg_major_version(&self) -> Option<u32> {
        parse_pg_major_version(self.server_version.as_deref()?)
    }

    /// Whether `pg_stat_io` is available (PG 16+).
    pub fn has_pg_stat_io(&self) -> bool {
        self.pg_major_version().is_some_and(|v| v >= 16)
    }

    /// Whether a connection pooler is active.
    #[allow(dead_code)]
    pub fn is_pooled(&self) -> bool {
        self.pooler.is_pooled()
    }

    /// Return a warning about pooler limitations, if applicable.
    #[allow(dead_code)]
    pub fn pooler_warning(&self) -> Option<&str> {
        self.pooler.pooler_warning()
    }
}

/// Parse the major version from a PG version string like `"16.2"` or `"14.11 (Ubuntu)"`.
fn parse_pg_major_version(version_str: &str) -> Option<u32> {
    version_str.split('.').next()?.trim().parse().ok()
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
    let pooler = detect_pooler(client, server_version.as_deref()).await;
    let managed_provider = detect_managed_provider(client).await;
    let standard_conforming_strings = detect_standard_conforming_strings(client).await;

    DbCapabilities {
        pg_ash,
        server_version,
        pooler,
        managed_provider,
        standard_conforming_strings,
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

/// Query the `standard_conforming_strings` GUC.
///
/// Returns `true` (the default) when the GUC is `on`, `false` when `off`.
/// On any error the safe default (`true`) is returned.
async fn detect_standard_conforming_strings(client: &tokio_postgres::Client) -> bool {
    detect_standard_conforming_strings_pub(client).await
}

/// Query `standard_conforming_strings` — public variant for re-detection
/// after `\c` reconnect.
pub async fn detect_standard_conforming_strings_pub(
    client: &tokio_postgres::Client,
) -> bool {
    match client
        .simple_query("SHOW standard_conforming_strings")
        .await
    {
        Ok(msgs) => msgs
            .iter()
            .find_map(|m| {
                if let tokio_postgres::SimpleQueryMessage::Row(row) = m {
                    row.get(0).map(|v| v == "on")
                } else {
                    None
                }
            })
            .unwrap_or(true),
        Err(_) => true,
    }
}

/// Read the `PostgreSQL` server version.
async fn detect_server_version(client: &tokio_postgres::Client) -> Option<String> {
    detect_server_version_pub(client).await
}

/// Read the `PostgreSQL` server version.
///
/// Exposed as `pub` so that the REPL can re-query the version after a `\c`
/// reconnect without running the full [`detect`] capability scan.
pub async fn detect_server_version_pub(client: &tokio_postgres::Client) -> Option<String> {
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

/// Extract the first row / first column from a `simple_query` result.
fn first_row_col(msgs: &[tokio_postgres::SimpleQueryMessage]) -> Option<String> {
    msgs.iter().find_map(|m| {
        if let tokio_postgres::SimpleQueryMessage::Row(row) = m {
            row.get(0).map(String::from)
        } else {
            None
        }
    })
}

/// Detect whether a connection pooler is sitting in front of the database.
///
/// Detection strategy:
/// 1. Attempt `SHOW pool_mode` — only `PgBouncer` exposes this GUC.
/// 2. Check `SHOW server_version` for Supavisor / `PgCat` version strings.
///
/// Errors from individual probes are swallowed; the function always returns
/// a valid [`PoolerType`].
async fn detect_pooler(
    client: &tokio_postgres::Client,
    server_version: Option<&str>,
) -> PoolerType {
    // 1. PgBouncer: try SHOW pool_mode (only PgBouncer responds to this).
    if let Ok(msgs) = client.simple_query("SHOW pool_mode").await {
        if let Some(pool_mode) = first_row_col(&msgs) {
            return PoolerType::PgBouncer { pool_mode };
        }
    }

    // 2. Supavisor / PgCat: check version banner from server_version GUC.
    if let Some(ver) = server_version {
        let lower = ver.to_lowercase();
        if lower.contains("supavisor") {
            return PoolerType::Supavisor;
        }
        if lower.contains("pgcat") {
            return PoolerType::PgCat;
        }
    }

    PoolerType::None
}

/// Detect whether this is a managed Postgres provider.
///
/// Detection strategy (each probe is independent; first match wins):
/// - RDS: `rds.extensions` GUC exists.
/// - Cloud SQL: any `cloudsql.*` GUC exists.
/// - Neon: any `neon.*` GUC exists.
/// - Supabase: `supabase_admin` role exists in `pg_roles`.
///
/// All probes are fault-tolerant; errors are silently ignored.
async fn detect_managed_provider(client: &tokio_postgres::Client) -> ManagedProvider {
    // RDS: check for the rds.extensions GUC.
    if let Ok(msgs) = client
        .simple_query("select name from pg_settings where name like 'rds.%' limit 1")
        .await
    {
        if msgs
            .iter()
            .any(|m| matches!(m, tokio_postgres::SimpleQueryMessage::Row(_)))
        {
            return ManagedProvider::Rds;
        }
    }

    // Cloud SQL: check for cloudsql.* GUCs.
    if let Ok(msgs) = client
        .simple_query("select name from pg_settings where name like 'cloudsql.%' limit 1")
        .await
    {
        if msgs
            .iter()
            .any(|m| matches!(m, tokio_postgres::SimpleQueryMessage::Row(_)))
        {
            return ManagedProvider::CloudSql;
        }
    }

    // Neon: check for neon.* GUCs.
    if let Ok(msgs) = client
        .simple_query("select name from pg_settings where name like 'neon.%' limit 1")
        .await
    {
        if msgs
            .iter()
            .any(|m| matches!(m, tokio_postgres::SimpleQueryMessage::Row(_)))
        {
            return ManagedProvider::Neon;
        }
    }

    // Supabase: check for the supabase_admin role.
    if let Ok(msgs) = client
        .simple_query("select 1 from pg_catalog.pg_roles where rolname = 'supabase_admin'")
        .await
    {
        if msgs
            .iter()
            .any(|m| matches!(m, tokio_postgres::SimpleQueryMessage::Row(_)))
        {
            return ManagedProvider::Supabase;
        }
    }

    ManagedProvider::None
}

/// Query whether the current session role is a superuser.
///
/// Uses `current_setting('is_superuser')` which returns `'on'` for
/// superusers and `'off'` for regular roles.  Defaults to `false` on
/// any error so the prompt degrades gracefully.
pub async fn detect_superuser(client: &tokio_postgres::Client) -> bool {
    match client
        .simple_query("select current_setting('is_superuser')")
        .await
    {
        Ok(msgs) => first_row_col(&msgs)
            .as_deref()
            .is_some_and(|v| v.eq_ignore_ascii_case("on")),
        Err(_) => false,
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
        assert!(!caps.is_pooled());
        assert_eq!(caps.managed_provider, ManagedProvider::None);
        // SCS defaults to true (PostgreSQL default since PG 9.1).
        assert!(caps.standard_conforming_strings);
    }

    #[test]
    fn pg_ash_available_without_version() {
        let status = PgAshStatus::Available { version: None };
        assert!(status.is_available());
    }

    #[test]
    fn parse_pg_major_version_simple() {
        assert_eq!(parse_pg_major_version("16.2"), Some(16));
        assert_eq!(parse_pg_major_version("14.11"), Some(14));
        assert_eq!(parse_pg_major_version("18.0"), Some(18));
    }

    #[test]
    fn parse_pg_major_version_with_suffix() {
        assert_eq!(
            parse_pg_major_version("14.11 (Ubuntu 14.11-1.pgdg22.04+1)"),
            Some(14)
        );
    }

    #[test]
    fn parse_pg_major_version_invalid() {
        assert_eq!(parse_pg_major_version(""), None);
        assert_eq!(parse_pg_major_version("abc"), None);
    }

    #[test]
    fn has_pg_stat_io_pg16() {
        let caps = DbCapabilities {
            server_version: Some("16.2".to_owned()),
            ..Default::default()
        };
        assert!(caps.has_pg_stat_io());
    }

    #[test]
    fn has_pg_stat_io_pg14() {
        let caps = DbCapabilities {
            server_version: Some("14.11".to_owned()),
            ..Default::default()
        };
        assert!(!caps.has_pg_stat_io());
    }

    #[test]
    fn has_pg_stat_io_no_version() {
        let caps = DbCapabilities::default();
        assert!(!caps.has_pg_stat_io());
    }

    // -- PoolerType tests ----------------------------------------------------

    #[test]
    fn pooler_type_default_is_none() {
        assert_eq!(PoolerType::default(), PoolerType::None);
    }

    #[test]
    fn pooler_type_none_is_not_pooled() {
        assert!(!PoolerType::None.is_pooled());
    }

    #[test]
    fn pooler_type_pgbouncer_is_pooled() {
        let p = PoolerType::PgBouncer {
            pool_mode: "transaction".to_owned(),
        };
        assert!(p.is_pooled());
    }

    #[test]
    fn pooler_type_supavisor_is_pooled() {
        assert!(PoolerType::Supavisor.is_pooled());
    }

    #[test]
    fn pooler_type_pgcat_is_pooled() {
        assert!(PoolerType::PgCat.is_pooled());
    }

    #[test]
    fn pooler_warning_none_is_none() {
        assert!(PoolerType::None.pooler_warning().is_none());
    }

    #[test]
    fn pooler_warning_pgbouncer_session_mode_is_none() {
        let p = PoolerType::PgBouncer {
            pool_mode: "session".to_owned(),
        };
        assert!(p.pooler_warning().is_none());
    }

    #[test]
    fn pooler_warning_pgbouncer_transaction_mode_has_warning() {
        let p = PoolerType::PgBouncer {
            pool_mode: "transaction".to_owned(),
        };
        assert!(p.pooler_warning().is_some());
    }

    #[test]
    fn pooler_warning_supavisor_has_warning() {
        assert!(PoolerType::Supavisor.pooler_warning().is_some());
    }

    #[test]
    fn pooler_warning_pgcat_has_warning() {
        assert!(PoolerType::PgCat.pooler_warning().is_some());
    }

    #[test]
    fn db_capabilities_is_pooled_delegates_to_pooler() {
        let caps = DbCapabilities {
            pooler: PoolerType::PgBouncer {
                pool_mode: "transaction".to_owned(),
            },
            ..Default::default()
        };
        assert!(caps.is_pooled());
        assert!(caps.pooler_warning().is_some());
    }

    // -- ManagedProvider tests -----------------------------------------------

    #[test]
    fn managed_provider_default_is_none() {
        assert_eq!(ManagedProvider::default(), ManagedProvider::None);
    }

    #[test]
    fn managed_provider_variants_distinct() {
        assert_ne!(ManagedProvider::Rds, ManagedProvider::None);
        assert_ne!(ManagedProvider::CloudSql, ManagedProvider::Rds);
        assert_ne!(ManagedProvider::Supabase, ManagedProvider::CloudSql);
        assert_ne!(ManagedProvider::Neon, ManagedProvider::Supabase);
    }
}
