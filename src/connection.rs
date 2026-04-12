//! Postgres wire-protocol connection and authentication.
//!
//! Resolves connection parameters from CLI flags, positional arguments,
//! URI / conninfo strings, `pg_service.conf`, environment variables,
//! `.pgpass`, and defaults.
//! Then establishes a `tokio-postgres` connection with optional TLS.
//!
//! ## Multi-host support
//!
//! Multiple hosts may be specified via:
//! - CLI flags: `rpg -h host1,host2 -p 5432` or `rpg -h host1,host2 -p 5432,5433`
//! - URI: `postgresql://h1,h2,h3/db` or `postgresql://h1:5432,h2:5433/db`
//! - conninfo: `host=h1,h2,h3 port=5432,5433`
//!
//! Hosts are tried in order until one accepts a connection that satisfies
//! the requested `target_session_attrs`.
//!
//! ## `target_session_attrs`
//!
//! After connecting, the session is verified against the requested attribute.
//! Configured via:
//! - `PGTARGETSESSIONATTRS` environment variable
//! - `target_session_attrs` URI query parameter
//! - `target_session_attrs` conninfo key

use std::collections::HashMap;
use std::env;
use std::fmt;
use std::future::Future;
use std::io;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, Error as RustlsError, SignatureScheme};
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio_postgres::config::SslMode as TokioSslMode;
use tokio_postgres::tls::{ChannelBinding, MakeTlsConnect, TlsConnect};
use tokio_postgres::Client;
use tokio_rustls::client::TlsStream;
use tokio_rustls::TlsConnector;

// ---------------------------------------------------------------------------
// Public error types
// ---------------------------------------------------------------------------

/// Errors specific to connection establishment.
#[derive(Debug, Error)]
pub enum ConnectionError {
    #[error("connection to server at \"{host}\", port {port} failed: {reason}")]
    ConnectionFailed {
        host: String,
        port: u16,
        reason: String,
    },

    #[error("authentication failed for user \"{user}\": {reason}")]
    AuthenticationFailed { user: String, reason: String },

    #[error("server requires SSL but sslmode=disable")]
    SslRequired,

    /// Server does not support SSL/TLS (sslmode=require/verify-ca/verify-full
    /// against a server with no TLS configured).
    #[error("SSL error: server does not support SSL")]
    SslNotSupported,

    /// Certificate verification failed (verify-ca or verify-full).
    #[error("SSL error: certificate verification failed: {0}")]
    SslCertVerificationFailed(String),

    /// General TLS failure (handshake or other TLS-layer error).
    #[error("SSL error: {0}")]
    TlsError(String),

    #[error("pgpass error: {0}")]
    PgpassError(String),

    #[error("invalid connection string: {0}")]
    InvalidConnectionString(String),

    #[error("cannot load SSL root certificate: {0}")]
    SslRootCertError(String),

    #[error("cannot load SSL client certificate or key: {0}")]
    SslClientCertError(String),

    #[error("service file error: {0}")]
    ServiceFileError(String),

    /// All hosts were tried but none satisfied `target_session_attrs`.
    #[error(
        "no suitable host found: tried {tried} host(s), \
         none matched target_session_attrs={attrs}"
    )]
    NoSuitableHost { tried: usize, attrs: String },

    /// Unknown value supplied for `target_session_attrs`.
    #[error("invalid target_session_attrs value: \"{0}\"")]
    InvalidTargetSessionAttrs(String),
}

// ---------------------------------------------------------------------------
// TLS session info
// ---------------------------------------------------------------------------

/// Negotiated TLS protocol version and cipher suite, captured after handshake.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TlsInfo {
    /// Protocol version string, e.g. `"TLSv1.3"` or `"TLSv1.2"`.
    pub protocol: String,
    /// Cipher suite name in IANA format, e.g. `"TLS_AES_256_GCM_SHA384"`.
    pub cipher: String,
}

impl TlsInfo {
    /// Format the SSL status line exactly as psql does:
    /// `SSL connection (protocol: TLSv1.3, cipher: TLS_AES_256_GCM_SHA384,
    /// compression: off)`
    pub fn status_line(&self) -> String {
        format!(
            "SSL connection (protocol: {}, cipher: {}, compression: off)",
            self.protocol, self.cipher,
        )
    }
}

/// Convert a rustls `ProtocolVersion` to the psql display string.
///
/// rustls uses `TLSv1_2` / `TLSv1_3`; psql shows `TLSv1.2` / `TLSv1.3`.
fn protocol_version_str(v: rustls::ProtocolVersion) -> String {
    match v.as_str() {
        Some(s) => s.replace('_', "."),
        None => format!("TLS(0x{:04x})", u16::from(v)),
    }
}

/// Convert a rustls `CipherSuite` to the IANA name used by psql.
///
/// rustls names TLS 1.3 suites as `TLS13_AES_256_GCM_SHA384`; IANA (and psql)
/// use `TLS_AES_256_GCM_SHA384`.  TLS 1.2 suites already start with `TLS_`.
fn cipher_suite_str(cs: rustls::CipherSuite) -> String {
    match cs.as_str() {
        Some(s) => {
            // rustls prefixes TLS 1.3 suites with "TLS13_"; IANA uses "TLS_".
            if let Some(rest) = s.strip_prefix("TLS13_") {
                format!("TLS_{rest}")
            } else {
                s.to_owned()
            }
        }
        None => format!("CipherSuite(0x{:04x})", u16::from(cs)),
    }
}

// ---------------------------------------------------------------------------
// SSL mode
// ---------------------------------------------------------------------------

/// Parsed SSL mode from user input.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SslMode {
    Disable,
    Allow,
    #[default]
    Prefer,
    Require,
    /// TLS required; server certificate verified against CA but hostname
    /// is NOT checked.
    VerifyCa,
    /// TLS required; server certificate verified and hostname matched.
    VerifyFull,
}

impl SslMode {
    /// Parse from a string value (case-insensitive).
    pub fn parse(s: &str) -> Result<Self, ConnectionError> {
        match s.to_lowercase().as_str() {
            "disable" => Ok(Self::Disable),
            "allow" => Ok(Self::Allow),
            "prefer" => Ok(Self::Prefer),
            "require" => Ok(Self::Require),
            "verify-ca" => Ok(Self::VerifyCa),
            "verify-full" => Ok(Self::VerifyFull),
            other => Err(ConnectionError::InvalidConnectionString(format!(
                "unknown sslmode: {other}"
            ))),
        }
    }
}

// ---------------------------------------------------------------------------
// Target session attributes
// ---------------------------------------------------------------------------

/// Specifies which session properties a candidate host must satisfy.
///
/// Mirrors the `target_session_attrs` libpq parameter.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TargetSessionAttrs {
    /// Accept any connection (default).
    #[default]
    Any,
    /// The session must be read-write (`transaction_read_only = off`).
    ReadWrite,
    /// The session must be read-only (`transaction_read_only = on`).
    ReadOnly,
    /// The host must be the primary (`transaction_read_only = off`).
    Primary,
    /// The host must be a standby (`pg_is_in_recovery() = true`).
    Standby,
    /// Prefer a standby; fall back to any host if none available.
    PreferStandby,
}

impl TargetSessionAttrs {
    /// Parse from a string value (case-insensitive, hyphens and underscores
    /// both accepted).
    pub fn parse(s: &str) -> Result<Self, ConnectionError> {
        match s.to_lowercase().replace('-', "_").as_str() {
            "any" => Ok(Self::Any),
            "read_write" => Ok(Self::ReadWrite),
            "read_only" => Ok(Self::ReadOnly),
            "primary" => Ok(Self::Primary),
            "standby" => Ok(Self::Standby),
            "prefer_standby" => Ok(Self::PreferStandby),
            _ => Err(ConnectionError::InvalidTargetSessionAttrs(s.to_owned())),
        }
    }

    /// Human-readable name used in error messages.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Any => "any",
            Self::ReadWrite => "read-write",
            Self::ReadOnly => "read-only",
            Self::Primary => "primary",
            Self::Standby => "standby",
            Self::PreferStandby => "prefer-standby",
        }
    }
}

impl fmt::Display for TargetSessionAttrs {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

// ---------------------------------------------------------------------------
// Resolved connection parameters
// ---------------------------------------------------------------------------

/// Fully-resolved connection parameters ready for use.
#[derive(Clone)]
pub struct ConnParams {
    pub host: String,
    pub port: u16,
    /// All candidate `(host, port)` pairs for a multi-host connection.
    ///
    /// When a single host is given this contains exactly one entry.  The
    /// values here are tried in order by [`connect`].  After a successful
    /// connection, `host` and `port` are updated to reflect the host that
    /// was actually used.
    pub hosts: Vec<(String, u16)>,
    /// Requested session attribute filter.
    pub target_session_attrs: TargetSessionAttrs,
    pub user: String,
    pub dbname: String,
    pub password: Option<String>,
    pub sslmode: SslMode,
    /// Path to a PEM file containing trusted CA certificate(s).
    ///
    /// Used by `sslmode=verify-ca` and `sslmode=verify-full`.  When `None`
    /// the built-in Mozilla/webpki root bundle is used.
    pub ssl_root_cert: Option<String>,
    /// Path to the client certificate PEM file (`PGSSLCERT` / `sslcert`).
    ///
    /// Used together with `ssl_key` for mutual TLS (client certificate auth).
    /// Only effective with `sslmode=verify-ca` or `sslmode=verify-full`.
    pub ssl_cert: Option<String>,
    /// Path to the client private key PEM file (`PGSSLKEY` / `sslkey`).
    ///
    /// Must be provided alongside `ssl_cert`.  If only one of `ssl_cert` /
    /// `ssl_key` is set a warning is emitted and no client cert is used.
    pub ssl_key: Option<String>,
    pub application_name: String,
    pub connect_timeout: Option<u64>,
    /// Server-side GUC options sent at connection startup via the `options`
    /// startup parameter (equivalent to `PGOPTIONS` / `options` conninfo key).
    pub options: Option<String>,
    /// TLS session details captured after the handshake.
    ///
    /// `None` when `sslmode=disable` or when `sslmode=prefer` fell back to a
    /// plain connection.  `Some` when the TLS handshake completed, containing
    /// the negotiated protocol version (e.g. `"TLSv1.3"`) and cipher suite
    /// (e.g. `"TLS_AES_256_GCM_SHA384"`).
    pub tls_info: Option<TlsInfo>,
    /// The numeric IP address that the TCP connection was made to, if known.
    ///
    /// `None` for Unix-socket connections or when DNS resolution was not
    /// attempted.  When `host` is already a numeric IP this stays `None`
    /// (there is nothing extra to show).  psql shows this as
    /// `(address "127.0.0.1")` after the hostname in `\conninfo` output.
    pub resolved_addr: Option<String>,
}

/// Custom `Debug` implementation that masks the password field.
impl fmt::Debug for ConnParams {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConnParams")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("hosts", &self.hosts)
            .field("target_session_attrs", &self.target_session_attrs)
            .field("user", &self.user)
            .field("dbname", &self.dbname)
            .field("password", &self.password.as_ref().map(|_| "***"))
            .field("sslmode", &self.sslmode)
            .field("ssl_root_cert", &self.ssl_root_cert)
            .field("ssl_cert", &self.ssl_cert)
            .field("ssl_key", &self.ssl_key)
            .field("application_name", &self.application_name)
            .field("connect_timeout", &self.connect_timeout)
            .field("options", &self.options)
            .field("tls_info", &self.tls_info)
            .field("resolved_addr", &self.resolved_addr)
            .finish()
    }
}

impl Default for ConnParams {
    fn default() -> Self {
        let (host, port) = default_host_port();
        Self {
            hosts: vec![(host.clone(), port)],
            host,
            port,
            target_session_attrs: TargetSessionAttrs::default(),
            user: default_user(),
            dbname: String::new(), // filled in by resolve — set to user
            password: None,
            sslmode: SslMode::default(),
            ssl_root_cert: None,
            ssl_cert: None,
            ssl_key: None,
            application_name: "rpg".to_owned(),
            connect_timeout: None,
            options: None,
            tls_info: None,
            resolved_addr: None,
        }
    }
}

/// Return the default `(host, port)` pair for a Unix socket connection.
///
/// On Unix, scans well-known socket directories (`/var/run/postgresql`,
/// `/tmp`) for `PostgreSQL` Unix-domain socket files (`.s.PGSQL.<port>`).
///
/// - **Fast path**: checks port 5432 first (O(1)) for libpq compatibility.
/// - **Slow path**: scans the directory for any `.s.PGSQL.<N>` socket and
///   returns the lowest-numbered port for determinism.
///
/// Both paths verify the candidate path is actually a socket (not a regular
/// file or stale `.lock` entry).
///
/// Falls back to `("localhost", 5432)` when no socket is found or on
/// non-Unix platforms.
fn default_host_port() -> (String, u16) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileTypeExt;

        for dir in &["/var/run/postgresql", "/tmp"] {
            let path = PathBuf::from(dir);

            // Fast path: the standard port 5432 socket.
            if path
                .join(".s.PGSQL.5432")
                .metadata()
                .map(|m| m.file_type().is_socket())
                .unwrap_or(false)
            {
                return ((*dir).to_owned(), 5432);
            }

            // Slow path: scan for any .s.PGSQL.<port> socket, pick lowest
            // port for determinism (read_dir order is undefined).
            let mut found_ports: Vec<u16> = Vec::new();
            if let Ok(entries) = std::fs::read_dir(&path) {
                for entry in entries.flatten() {
                    let name = entry.file_name();
                    let name_str = name.to_string_lossy();
                    if let Some(port_str) = name_str.strip_prefix(".s.PGSQL.") {
                        // Exclude lock files (.s.PGSQL.<port>.lock).
                        if !port_str.contains('.') {
                            if let Ok(port) = port_str.parse::<u16>() {
                                if entry
                                    .path()
                                    .metadata()
                                    .map(|m| m.file_type().is_socket())
                                    .unwrap_or(false)
                                {
                                    found_ports.push(port);
                                }
                            }
                        }
                    }
                }
            }
            found_ports.sort_unstable();
            if let Some(&port) = found_ports.first() {
                return ((*dir).to_owned(), port);
            }
        }
    }
    ("localhost".to_owned(), 5432)
}

/// Default user: `$USER` (or `$USERNAME` on Windows), falling back to
/// `"postgres"`.
fn default_user() -> String {
    env::var("USER")
        .or_else(|_| env::var("USERNAME"))
        .unwrap_or_else(|_| "postgres".to_owned())
}

/// Return `true` when `host` is already a numeric IP address (IPv4 or IPv6).
///
/// When the host is already an IP we skip DNS resolution in [`connect`]
/// because there is no hostname to resolve — `\conninfo` would have nothing
/// extra to display in the `(address "...")` clause.
fn is_numeric_addr(host: &str) -> bool {
    use std::net::IpAddr;
    host.parse::<IpAddr>().is_ok()
}

// ---------------------------------------------------------------------------
// CLI flags mirror (to decouple from clap types)
// ---------------------------------------------------------------------------

/// Subset of CLI flags that affect connection parameters.
///
/// This struct intentionally mirrors only the fields we need, so that
/// `connection.rs` doesn't depend on the `Cli` struct directly.
#[derive(Clone, Debug, Default)]
pub struct CliConnOpts {
    pub host: Option<String>,
    pub port: Option<u16>,
    /// Raw port string from CLI, used when `-p` supplies comma-separated ports
    /// for multi-host connections (`-h host1,host2 -p 5432,5433`).
    /// Takes precedence over `port` when parsing multi-host lists.
    pub port_str: Option<String>,
    pub username: Option<String>,
    pub dbname: Option<String>,
    pub dbname_pos: Option<String>,
    pub user_pos: Option<String>,
    pub host_pos: Option<String>,
    pub port_pos: Option<String>,
    pub force_password: bool,
    pub no_password: bool,
    /// SSL mode override from `--sslmode` CLI flag (highest priority).
    pub sslmode: Option<String>,
    /// SSH tunnel configuration, if any.
    ///
    /// When present, the connection is established through an SSH tunnel.
    pub ssh_tunnel: Option<crate::config::SshTunnelConfig>,
}

// ---------------------------------------------------------------------------
// Parameter resolution
// ---------------------------------------------------------------------------

/// Resolve connection parameters from CLI options, environment, and defaults.
///
/// Priority (highest first):
/// 1. Named CLI flags (`-h`, `-p`, `-U`, `-d`, `--sslmode`)
/// 2. Positional arguments
/// 3. URI format (`postgresql://…`)
/// 4. Key-value conninfo (`host=… port=…`)
/// 5. `pg_service.conf` service defaults
/// 6. Environment variables
/// 7. Defaults
pub fn resolve_params(opts: &CliConnOpts) -> Result<ConnParams, ConnectionError> {
    let mut params = ConnParams::default();

    // Check if the first positional argument is a URI or conninfo string.
    let mut uri_params: Option<UriParams> = None;
    let mut conninfo_params: Option<HashMap<String, String>> = None;

    if let Some(ref dbname_pos) = opts.dbname_pos {
        if dbname_pos.starts_with("postgresql://") || dbname_pos.starts_with("postgres://") {
            uri_params = Some(parse_uri(dbname_pos)?);
        } else if dbname_pos.contains('=') {
            conninfo_params = Some(parse_conninfo(dbname_pos)?);
        }
    }

    let is_plain_positional = uri_params.is_none() && conninfo_params.is_none();

    // Determine which service to look up.  The service name can come from:
    //   - `service=<name>` inside a conninfo string
    //   - `PGSERVICE` environment variable
    let service_name = conninfo_params
        .as_ref()
        .and_then(|c| c.get("service").cloned())
        .or_else(|| env::var("PGSERVICE").ok());

    // Load service defaults (may be empty / None if no service is requested
    // or the named service is not found).
    let svc = if let Some(ref name) = service_name {
        resolve_service(name)?
    } else {
        HashMap::new()
    };
    let svc_ref = if svc.is_empty() { None } else { Some(&svc) };

    let uri_ref = uri_params.as_ref();
    let ci_ref = conninfo_params.as_ref();

    // Compute the socket-based default once so that both resolve_host and
    // resolve_port use the same scan result (avoids a TOCTOU race where two
    // independent read_dir() calls could return different sockets).
    let (dflt_host, dflt_port) = default_host_port();

    resolve_host(
        &mut params,
        opts,
        uri_ref,
        ci_ref,
        svc_ref,
        is_plain_positional,
        dflt_host,
    );
    resolve_port(
        &mut params,
        opts,
        uri_ref,
        ci_ref,
        svc_ref,
        is_plain_positional,
        dflt_port,
    );
    resolve_user(
        &mut params,
        opts,
        uri_ref,
        ci_ref,
        svc_ref,
        is_plain_positional,
    );
    resolve_dbname(
        &mut params,
        opts,
        uri_ref,
        ci_ref,
        svc_ref,
        is_plain_positional,
    );

    // Password (from URI / conninfo / service / env only;
    // pgpass + prompt happen later).
    params.password = uri_ref
        .and_then(|u| u.password.clone())
        .or_else(|| {
            conninfo_params
                .as_ref()
                .and_then(|c| c.get("password").cloned())
        })
        .or_else(|| svc_ref.and_then(|s| s.get("password").cloned()))
        .or_else(|| env::var("PGPASSWORD").ok());

    resolve_sslmode(&mut params, opts, uri_ref, ci_ref, svc_ref);
    resolve_ssl_root_cert(&mut params, uri_ref, ci_ref, svc_ref);
    resolve_ssl_cert(&mut params, uri_ref, ci_ref, svc_ref);
    resolve_ssl_key(&mut params, uri_ref, ci_ref, svc_ref);
    resolve_app_name(&mut params, uri_ref, ci_ref, svc_ref);
    resolve_options(&mut params, uri_ref, ci_ref, svc_ref);

    // Connect timeout: URI query params, then conninfo, then service, then env.
    params.connect_timeout = uri_ref
        .and_then(|u| u.connect_timeout)
        .or_else(|| {
            conninfo_params
                .as_ref()
                .and_then(|c| c.get("connect_timeout").and_then(|v| v.parse().ok()))
        })
        .or_else(|| svc_ref.and_then(|s| s.get("connect_timeout").and_then(|v| v.parse().ok())))
        .or_else(|| {
            env::var("PGCONNECT_TIMEOUT")
                .ok()
                .and_then(|v| v.parse().ok())
        });

    // Multi-host list — built after host/port are resolved.
    resolve_hosts(&mut params, uri_ref, ci_ref, Some(opts));

    // target_session_attrs — URI query param, conninfo key, then env.
    resolve_target_session_attrs(&mut params, uri_ref, ci_ref)?;

    Ok(params)
}

fn resolve_host(
    params: &mut ConnParams,
    opts: &CliConnOpts,
    uri: Option<&UriParams>,
    conninfo: Option<&HashMap<String, String>>,
    svc: Option<&HashMap<String, String>>,
    is_plain: bool,
    default_host: String,
) {
    params.host = opts
        .host
        .clone()
        .or_else(|| {
            if is_plain {
                opts.host_pos.clone()
            } else {
                None
            }
        })
        .or_else(|| uri.and_then(|u| u.host.clone()))
        .or_else(|| conninfo.and_then(|c| c.get("host").cloned()))
        .or_else(|| svc.and_then(|s| s.get("host").cloned()))
        .or_else(|| env::var("PGHOST").ok())
        .unwrap_or(default_host);
}

fn resolve_port(
    params: &mut ConnParams,
    opts: &CliConnOpts,
    uri: Option<&UriParams>,
    conninfo: Option<&HashMap<String, String>>,
    svc: Option<&HashMap<String, String>>,
    is_plain: bool,
    default_port: u16,
) {
    params.port = opts
        .port
        .or_else(|| {
            if is_plain {
                opts.port_pos.as_ref().and_then(|p| p.parse().ok())
            } else {
                None
            }
        })
        .or_else(|| uri.and_then(|u| u.port))
        .or_else(|| conninfo.and_then(|c| c.get("port").and_then(|p| p.parse().ok())))
        .or_else(|| svc.and_then(|s| s.get("port").and_then(|p| p.parse().ok())))
        .or_else(|| env::var("PGPORT").ok().and_then(|p| p.parse().ok()))
        .unwrap_or(default_port);
}

fn resolve_user(
    params: &mut ConnParams,
    opts: &CliConnOpts,
    uri: Option<&UriParams>,
    conninfo: Option<&HashMap<String, String>>,
    svc: Option<&HashMap<String, String>>,
    is_plain: bool,
) {
    params.user = opts
        .username
        .clone()
        .or_else(|| {
            if is_plain {
                opts.user_pos.clone()
            } else {
                None
            }
        })
        .or_else(|| uri.and_then(|u| u.user.clone()))
        .or_else(|| conninfo.and_then(|c| c.get("user").cloned()))
        .or_else(|| svc.and_then(|s| s.get("user").cloned()))
        .or_else(|| env::var("PGUSER").ok())
        .unwrap_or_else(default_user);
}

fn resolve_dbname(
    params: &mut ConnParams,
    opts: &CliConnOpts,
    uri: Option<&UriParams>,
    conninfo: Option<&HashMap<String, String>>,
    svc: Option<&HashMap<String, String>>,
    is_plain: bool,
) {
    params.dbname = opts
        .dbname
        .clone()
        .or_else(|| {
            if is_plain {
                opts.dbname_pos.clone()
            } else {
                None
            }
        })
        .or_else(|| uri.and_then(|u| u.dbname.clone()))
        .or_else(|| conninfo.and_then(|c| c.get("dbname").cloned()))
        .or_else(|| svc.and_then(|s| s.get("dbname").cloned()))
        .or_else(|| env::var("PGDATABASE").ok())
        .unwrap_or_else(|| params.user.clone());
}

fn resolve_sslmode(
    params: &mut ConnParams,
    opts: &CliConnOpts,
    uri: Option<&UriParams>,
    conninfo: Option<&HashMap<String, String>>,
    svc: Option<&HashMap<String, String>>,
) {
    // CLI flag has highest priority.
    params.sslmode = opts
        .sslmode
        .as_deref()
        .and_then(|s| SslMode::parse(s).ok())
        .or_else(|| uri.and_then(|u| u.sslmode))
        .or_else(|| {
            conninfo
                .and_then(|c| c.get("sslmode"))
                .and_then(|s| SslMode::parse(s).ok())
        })
        .or_else(|| {
            svc.and_then(|s| s.get("sslmode"))
                .and_then(|s| SslMode::parse(s).ok())
        })
        .or_else(|| {
            env::var("PGSSLMODE")
                .ok()
                .and_then(|s| SslMode::parse(&s).ok())
        })
        .unwrap_or_default();
}

fn resolve_app_name(
    params: &mut ConnParams,
    uri: Option<&UriParams>,
    conninfo: Option<&HashMap<String, String>>,
    svc: Option<&HashMap<String, String>>,
) {
    params.application_name = uri
        .and_then(|u| u.application_name.clone())
        .or_else(|| conninfo.and_then(|c| c.get("application_name").cloned()))
        .or_else(|| svc.and_then(|s| s.get("application_name").cloned()))
        .or_else(|| env::var("PGAPPNAME").ok())
        .unwrap_or_else(|| "rpg".to_owned());
}

fn resolve_ssl_root_cert(
    params: &mut ConnParams,
    uri: Option<&UriParams>,
    conninfo: Option<&HashMap<String, String>>,
    svc: Option<&HashMap<String, String>>,
) {
    params.ssl_root_cert = uri
        .and_then(|u| u.ssl_root_cert.clone())
        .or_else(|| conninfo.and_then(|c| c.get("sslrootcert").cloned()))
        .or_else(|| svc.and_then(|s| s.get("sslrootcert").cloned()))
        .or_else(|| env::var("PGSSLROOTCERT").ok());
}

fn resolve_ssl_cert(
    params: &mut ConnParams,
    uri: Option<&UriParams>,
    conninfo: Option<&HashMap<String, String>>,
    svc: Option<&HashMap<String, String>>,
) {
    params.ssl_cert = uri
        .and_then(|u| u.ssl_cert.clone())
        .or_else(|| conninfo.and_then(|c| c.get("sslcert").cloned()))
        .or_else(|| svc.and_then(|s| s.get("sslcert").cloned()))
        .or_else(|| env::var("PGSSLCERT").ok());
}

fn resolve_ssl_key(
    params: &mut ConnParams,
    uri: Option<&UriParams>,
    conninfo: Option<&HashMap<String, String>>,
    svc: Option<&HashMap<String, String>>,
) {
    params.ssl_key = uri
        .and_then(|u| u.ssl_key.clone())
        .or_else(|| conninfo.and_then(|c| c.get("sslkey").cloned()))
        .or_else(|| svc.and_then(|s| s.get("sslkey").cloned()))
        .or_else(|| env::var("PGSSLKEY").ok());
}

fn resolve_options(
    params: &mut ConnParams,
    uri: Option<&UriParams>,
    conninfo: Option<&HashMap<String, String>>,
    svc: Option<&HashMap<String, String>>,
) {
    params.options = uri
        .and_then(|u| u.options.clone())
        .or_else(|| conninfo.and_then(|c| c.get("options").cloned()))
        .or_else(|| svc.and_then(|s| s.get("options").cloned()))
        .or_else(|| env::var("PGOPTIONS").ok());
}

/// Build the canonical multi-host list in `params.hosts`.
///
/// Priority:
/// 1. Multi-host from URI (already parsed into `uri.hosts`)
/// 2. Multi-host from conninfo (only when conninfo explicitly has multiple hosts)
/// 3. Multi-host from CLI -h flag (takes priority over single-host conninfo)
/// 4. Single host fallback
fn resolve_hosts(
    params: &mut ConnParams,
    uri: Option<&UriParams>,
    conninfo: Option<&HashMap<String, String>>,
    opts: Option<&CliConnOpts>,
) {
    // URI multi-host takes precedence.
    if let Some(u) = uri {
        if u.hosts.len() > 1 {
            params.hosts.clone_from(&u.hosts);
            return;
        }
    }

    // conninfo multi-host: `host=h1,h2,h3 port=5432,5433`
    if let Some(ci) = conninfo {
        if let Some(host_val) = ci.get("host") {
            let host_parts: Vec<&str> = host_val.split(',').map(str::trim).collect();
            if host_parts.len() > 1 {
                // Parse ports — comma-separated; last port is reused.
                let port_parts: Vec<u16> = ci
                    .get("port")
                    .map(|p| {
                        p.split(',')
                            .filter_map(|s| s.trim().parse::<u16>().ok())
                            .collect()
                    })
                    .unwrap_or_default();

                let default_port = params.port; // already resolved
                let mut last_port = default_port;
                let mut host_list: Vec<(String, u16)> = Vec::with_capacity(host_parts.len());
                for (i, h) in host_parts.iter().enumerate() {
                    if let Some(&p) = port_parts.get(i) {
                        last_port = p;
                    }
                    host_list.push(((*h).to_owned(), last_port));
                }
                params.hosts = host_list;
                return;
            }
        }
    }

    // CLI multi-host: `-h host1,host2[,host3] [-p port1[,port2]]`
    // Applies to both named `--host` and positional HOST argument.
    if let Some(o) = opts {
        let cli_host = o.host.as_deref().or(o.host_pos.as_deref());
        if let Some(h) = cli_host {
            let host_parts: Vec<&str> = h.split(',').map(str::trim).collect();
            if host_parts.len() > 1 {
                // Ports: prefer port_str (raw string, may be comma-separated),
                // fall back to the already-parsed single u16 in opts.port /
                // opts.port_pos, then to params.port (global default).
                //
                // Note: CLI -h takes priority over single-host conninfo — the
                // multi-host path is entered only when -h supplies multiple
                // hosts, so any single-host conninfo value is superseded here.
                let raw_ports = o
                    .port_str
                    .as_deref()
                    .or(o.port_pos.as_deref())
                    .unwrap_or("");
                // Guard against invalid port values: parse each token and warn
                // loudly if any is not a valid u16.  Silently dropping an
                // invalid token would cause the wrong port to be used for the
                // remaining hosts, which is worse than falling back to the
                // single-host path.
                let mut port_parse_ok = true;
                let port_parts: Vec<u16> = raw_ports
                    .split(',')
                    .filter(|s| !s.trim().is_empty())
                    .map(|s| {
                        s.trim().parse::<u16>().unwrap_or_else(|_| {
                            eprintln!("rpg: invalid port value '{}' in -p", s.trim());
                            port_parse_ok = false;
                            0
                        })
                    })
                    .collect();
                if !port_parse_ok {
                    // At least one port token was invalid.  Populate hosts from
                    // the already-resolved single-host fields so the caller
                    // always gets a valid host list, then return.
                    params.hosts = vec![(params.host.clone(), params.port)];
                    return;
                }

                let default_port = params.port;
                let mut last_port = default_port;
                let mut host_list: Vec<(String, u16)> = Vec::with_capacity(host_parts.len());
                for (i, host_part) in host_parts.iter().enumerate() {
                    if let Some(&p) = port_parts.get(i) {
                        last_port = p;
                    }
                    host_list.push(((*host_part).to_owned(), last_port));
                }
                // Update params.host/port to reflect first entry (backward-compat).
                params.host.clone_from(&host_list[0].0);
                params.port = host_list[0].1;
                params.hosts = host_list;
                return;
            }
        }
    }

    // Fallback: single host from the already-resolved fields.
    params.hosts = vec![(params.host.clone(), params.port)];
}

fn resolve_target_session_attrs(
    params: &mut ConnParams,
    uri: Option<&UriParams>,
    conninfo: Option<&HashMap<String, String>>,
) -> Result<(), ConnectionError> {
    params.target_session_attrs = if let Some(tsa) = uri.and_then(|u| u.target_session_attrs) {
        tsa
    } else if let Some(val) = conninfo.and_then(|c| c.get("target_session_attrs")) {
        TargetSessionAttrs::parse(val)?
    } else if let Ok(val) = env::var("PGTARGETSESSIONATTRS") {
        TargetSessionAttrs::parse(&val)?
    } else {
        TargetSessionAttrs::Any
    };
    Ok(())
}

// ---------------------------------------------------------------------------
// URI parsing
// ---------------------------------------------------------------------------

/// Intermediate result of parsing a `postgresql://…` URI.
#[derive(Debug, Default)]
struct UriParams {
    host: Option<String>,
    port: Option<u16>,
    /// Parsed multi-host list.  When the URI authority contains comma-
    /// separated hosts, all entries land here (and `host`/`port` reflect
    /// only the *first* entry for backward-compat callers that don't look
    /// at `hosts`).
    hosts: Vec<(String, u16)>,
    user: Option<String>,
    password: Option<String>,
    dbname: Option<String>,
    sslmode: Option<SslMode>,
    ssl_root_cert: Option<String>,
    ssl_cert: Option<String>,
    ssl_key: Option<String>,
    application_name: Option<String>,
    connect_timeout: Option<u64>,
    options: Option<String>,
    target_session_attrs: Option<TargetSessionAttrs>,
}

/// Extract `sslcert`, `sslkey`, and `sslrootcert` from a connection URI or
/// connstring.
///
/// `tokio_postgres::Config` handles all other standard libpq parameters.
/// These three are the only ones it omits — it delegates TLS entirely to the
/// caller, so client certificates and the CA bundle must be extracted
/// separately.
///
/// Handles both URI query-string format (`?sslcert=...&sslkey=...`) and
/// key=value connstring format (`sslcert=... sslkey=...`).
fn extract_ssl_file_params(s: &str) -> (Option<String>, Option<String>, Option<String>) {
    let mut ssl_cert: Option<String> = None;
    let mut ssl_key: Option<String> = None;
    let mut ssl_root_cert: Option<String> = None;

    if s.starts_with("postgresql://") || s.starts_with("postgres://") {
        // URI: parameters are in the query string after '?'.
        let query = s.split_once('?').map_or("", |(_, q)| q);
        for pair in query.split('&') {
            if let Some((key, val)) = pair.split_once('=') {
                let val = percent_decode(val);
                match key {
                    "sslcert" => ssl_cert = Some(val),
                    "sslkey" => ssl_key = Some(val),
                    "sslrootcert" => ssl_root_cert = Some(val),
                    _ => {}
                }
            }
        }
    } else {
        // key=value connstring: scan the whole string.
        let mut rest = s.trim();
        while !rest.is_empty() {
            let Some(eq_pos) = rest.find('=') else {
                break;
            };
            let key = rest[..eq_pos].trim();
            rest = rest[eq_pos + 1..].trim_start();

            let value;
            if rest.starts_with('\'') {
                // Quoted value.
                let mut end = 1;
                let bytes = rest.as_bytes();
                let mut val = String::new();
                while end < bytes.len() {
                    if bytes[end] == b'\\' && end + 1 < bytes.len() {
                        val.push(char::from(bytes[end + 1]));
                        end += 2;
                    } else if bytes[end] == b'\'' {
                        end += 1;
                        break;
                    } else {
                        val.push(char::from(bytes[end]));
                        end += 1;
                    }
                }
                value = val;
                rest = rest[end..].trim_start();
            } else {
                let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
                value = rest[..end].to_owned();
                rest = rest[end..].trim_start();
            }

            match key {
                "sslcert" => ssl_cert = Some(value),
                "sslkey" => ssl_key = Some(value),
                "sslrootcert" => ssl_root_cert = Some(value),
                _ => {}
            }
        }
    }

    (ssl_cert, ssl_key, ssl_root_cert)
}

/// Build a sanitized copy of a URI with params that `tokio_postgres::Config`
/// does not support removed from the query string.
///
/// `tokio_postgres` only supports sslmode values `disable`, `prefer`,
/// `require` and `target_session_attrs` values `any`, `read-write`, `read-only`.
/// It also errors on unrecognised keys.  Strip all rpg-extended params so the
/// sanitized URI can be parsed by tokio-postgres without error.
///
/// Removed params: `sslcert`, `sslkey`, `sslrootcert`.
/// Rewritten params: `sslmode` when value is `allow`, `verify-ca`, or
/// `verify-full` (mapped to `require` so tokio-postgres accepts the string);
/// `target_session_attrs` when value is `primary`, `standby`, or
/// `prefer-standby` / `prefer_standby` (removed — resolved separately).
fn sanitize_uri_for_tokio(uri: &str) -> String {
    let (base, query) = match uri.split_once('?') {
        Some((b, q)) => (b, Some(q)),
        None => return uri.to_owned(),
    };

    let Some(query) = query else {
        return uri.to_owned();
    };

    let kept: Vec<String> = query
        .split('&')
        .filter_map(|pair| {
            let (key, val) = pair.split_once('=')?;
            match key {
                // Params tokio-postgres does not recognise — drop entirely.
                "sslcert" | "sslkey" | "sslrootcert" => None,
                // sslmode: map rpg-extended values to `require` so the parse
                // succeeds.  The real value is captured from the raw URI.
                "sslmode" => {
                    let mapped = match val {
                        "allow" | "verify-ca" | "verify-full" => "require",
                        other => other,
                    };
                    Some(format!("sslmode={mapped}"))
                }
                // target_session_attrs values beyond tokio-postgres's subset —
                // remove; resolved later from the raw URI by parse_uri.
                "target_session_attrs" => match val {
                    "primary" | "standby" | "prefer-standby" | "prefer_standby" => None,
                    other => Some(format!("target_session_attrs={other}")),
                },
                // Everything else passes through unchanged.
                _ => Some(pair.to_owned()),
            }
        })
        .collect();

    if kept.is_empty() {
        base.to_owned()
    } else {
        format!("{}?{}", base, kept.join("&"))
    }
}

/// Parse a `postgresql://` or `postgres://` URI into individual fields.
///
/// Delegates to `tokio_postgres::Config::from_str()` for all standard libpq
/// parameters (host, port, multi-host, Unix socket paths, sslmode,
/// `application_name`, `connect_timeout`, `options`, `target_session_attrs`,
/// percent-encoding, etc.).
///
/// The only params tokio-postgres omits are `sslcert`, `sslkey`, and
/// `sslrootcert` — standard libpq params that tokio-postgres skips because it
/// delegates TLS entirely to the caller.  These are extracted from the raw URI
/// via `extract_ssl_file_params`.
///
/// rpg-extended `sslmode` values (`allow`, `verify-ca`, `verify-full`) and
/// `target_session_attrs` values (`primary`, `standby`, `prefer-standby`) are
/// also extracted from the raw URI before the sanitized copy is handed to
/// tokio-postgres.
fn parse_uri(uri: &str) -> Result<UriParams, ConnectionError> {
    use tokio_postgres::config::Host as TokioHost;

    // Extract params that tokio-postgres does not support, from the raw URI.
    let (ssl_cert, ssl_key, ssl_root_cert) = extract_ssl_file_params(uri);

    // Extract rpg-extended sslmode and target_session_attrs from the raw query
    // string before sanitizing, since sanitize_uri_for_tokio may rewrite them.
    let raw_sslmode = uri.split_once('?').and_then(|(_, q)| {
        q.split('&').find_map(|pair| {
            let (k, v) = pair.split_once('=')?;
            if k == "sslmode" {
                Some(percent_decode(v))
            } else {
                None
            }
        })
    });
    let raw_tsa = uri.split_once('?').and_then(|(_, q)| {
        q.split('&').find_map(|pair| {
            let (k, v) = pair.split_once('=')?;
            if k == "target_session_attrs" {
                Some(percent_decode(v))
            } else {
                None
            }
        })
    });

    // Build a sanitized URI that tokio-postgres can parse without errors.
    let sanitized = sanitize_uri_for_tokio(uri);

    // Delegate the bulk of the parsing to tokio-postgres.
    let pg_cfg = sanitized
        .parse::<tokio_postgres::Config>()
        .map_err(|e| ConnectionError::InvalidConnectionString(e.to_string()))?;

    // --- Reconstruct UriParams from the parsed Config ---

    let mut params = UriParams::default();

    // Hosts: tokio-postgres returns Host::Tcp(String) or Host::Unix(PathBuf).
    // Ports: empty slice = all default 5432; single entry = same for all hosts;
    //        N entries = per-host (tokio-postgres validates this invariant).
    let tp_hosts = pg_cfg.get_hosts();
    let tp_ports = pg_cfg.get_ports();

    if !tp_hosts.is_empty() {
        let port_for = |i: usize| -> u16 {
            match tp_ports.len() {
                0 => 5432,
                1 => tp_ports[0],
                _ => *tp_ports.get(i).unwrap_or(&5432),
            }
        };

        let mut host_list: Vec<(String, u16)> = Vec::with_capacity(tp_hosts.len());
        for (i, h) in tp_hosts.iter().enumerate() {
            let host_str = match h {
                TokioHost::Tcp(s) => s.clone(),
                #[cfg(unix)]
                TokioHost::Unix(p) => p.to_string_lossy().into_owned(),
            };
            host_list.push((host_str, port_for(i)));
        }

        // Populate the legacy single-host fields from the first entry.
        params.host = Some(host_list[0].0.clone());
        params.port = Some(host_list[0].1);
        params.hosts = host_list;
    }

    params.user = pg_cfg.get_user().map(str::to_owned);
    params.password = pg_cfg
        .get_password()
        .and_then(|b| std::str::from_utf8(b).ok())
        .map(str::to_owned);
    params.dbname = pg_cfg.get_dbname().map(str::to_owned);

    // sslmode: prefer the raw value from the URI (which may be an rpg-extended
    // variant like verify-ca/verify-full/allow) over the tokio-postgres parsed
    // value, since we sanitized the URI before parsing.
    params.sslmode = if let Some(ref s) = raw_sslmode {
        Some(SslMode::parse(s)?)
    } else {
        // Map the tokio-postgres SslMode back to rpg's SslMode.
        Some(match pg_cfg.get_ssl_mode() {
            TokioSslMode::Disable => SslMode::Disable,
            TokioSslMode::Require => SslMode::Require,
            // tokio-postgres SslMode is #[non_exhaustive]; Prefer and unknown variants map to Prefer.
            _ => SslMode::Prefer,
        })
    };

    params.application_name = pg_cfg.get_application_name().map(str::to_owned);
    params.connect_timeout = pg_cfg
        .get_connect_timeout()
        .map(std::time::Duration::as_secs);
    params.options = pg_cfg.get_options().map(str::to_owned);

    // target_session_attrs: prefer the raw URI value (rpg may have extended
    // values like primary/standby) over tokio-postgres's parsed subset.
    params.target_session_attrs = if let Some(ref s) = raw_tsa {
        Some(TargetSessionAttrs::parse(s)?)
    } else {
        use tokio_postgres::config::TargetSessionAttrs as TokioTsa;
        match pg_cfg.get_target_session_attrs() {
            TokioTsa::ReadWrite => Some(TargetSessionAttrs::ReadWrite),
            TokioTsa::ReadOnly => Some(TargetSessionAttrs::ReadOnly),
            // Any is the default; other non_exhaustive variants also map to None.
            _ => None,
        }
    };

    params.ssl_cert = ssl_cert;
    params.ssl_key = ssl_key;
    params.ssl_root_cert = ssl_root_cert;

    Ok(params)
}

/// Minimal percent-decoding for URI components.
fn percent_decode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.as_bytes().iter();
    while let Some(&b) = chars.next() {
        if b == b'%' {
            // A valid percent-encoded triplet requires exactly two hex digits
            // after the `%`. If fewer remain, pass the `%` through literally
            // instead of substituting NUL bytes.
            let Some(hi) = chars.next().copied() else {
                out.push('%');
                continue;
            };
            let Some(lo) = chars.next().copied() else {
                out.push('%');
                out.push(char::from(hi));
                continue;
            };
            let hex = [hi, lo];
            if let Ok(s) = std::str::from_utf8(&hex) {
                if let Ok(byte) = u8::from_str_radix(s, 16) {
                    out.push(char::from(byte));
                    continue;
                }
            }
            // Malformed hex digits; pass through literally.
            out.push('%');
            out.push(char::from(hi));
            out.push(char::from(lo));
        } else {
            out.push(char::from(b));
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Key-value conninfo parsing
// ---------------------------------------------------------------------------

/// Parse a key=value conninfo string.
///
/// Supports quoted values: `host='my host' port=5432`.
fn parse_conninfo(s: &str) -> Result<HashMap<String, String>, ConnectionError> {
    let err = |msg: String| ConnectionError::InvalidConnectionString(msg);
    let mut map = HashMap::new();
    let mut rest = s.trim();

    while !rest.is_empty() {
        // Find the key.
        let eq_pos = rest
            .find('=')
            .ok_or_else(|| err(format!("expected key=value in conninfo: {rest}")))?;
        let key = rest[..eq_pos].trim().to_owned();
        rest = rest[eq_pos + 1..].trim_start();

        // Parse value (possibly quoted).
        let value;
        if rest.starts_with('\'') {
            // Quoted value.
            let mut end = 1;
            let bytes = rest.as_bytes();
            let mut val = String::new();
            while end < bytes.len() {
                if bytes[end] == b'\\' && end + 1 < bytes.len() {
                    val.push(char::from(bytes[end + 1]));
                    end += 2;
                } else if bytes[end] == b'\'' {
                    end += 1;
                    break;
                } else {
                    val.push(char::from(bytes[end]));
                    end += 1;
                }
            }
            value = val;
            rest = rest[end..].trim_start();
        } else {
            // Unquoted value — ends at next whitespace.
            let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
            value = rest[..end].to_owned();
            rest = rest[end..].trim_start();
        }

        map.insert(key, value);
    }

    Ok(map)
}

// ---------------------------------------------------------------------------
// pg_service.conf support
// ---------------------------------------------------------------------------

/// Valid parameter keys recognised in a `pg_service.conf` service section.
///
/// Any other key found in the file is silently ignored, matching psql behaviour.
const SERVICE_VALID_KEYS: &[&str] = &[
    "host",
    "port",
    "dbname",
    "user",
    "password",
    "sslmode",
    "sslrootcert",
    "sslcert",
    "sslkey",
    "application_name",
    "connect_timeout",
    "options",
];

/// Return the path to the service file that should be consulted, in priority
/// order:
///
/// 1. `$PGSERVICEFILE` (explicit override)
/// 2. `~/.pg_service.conf` (user file)
/// 3. `$PGSYSCONFDIR/pg_service.conf`
/// 4. `/etc/postgresql-common/pg_service.conf` (system default)
///
/// The first path that exists is returned.  Returns `None` if no file is
/// found or if the home directory cannot be determined.
fn service_file_path() -> Option<PathBuf> {
    // 1. Explicit env override.
    if let Ok(p) = env::var("PGSERVICEFILE") {
        return Some(PathBuf::from(p));
    }

    // 2. User service file.
    if let Some(home) = dirs::home_dir() {
        let user_path = home.join(".pg_service.conf");
        if user_path.exists() {
            return Some(user_path);
        }
    }

    // 3. $PGSYSCONFDIR.
    if let Ok(dir) = env::var("PGSYSCONFDIR") {
        let p = PathBuf::from(dir).join("pg_service.conf");
        if p.exists() {
            return Some(p);
        }
    }

    // 4. Well-known system path.
    let sys = PathBuf::from("/etc/postgresql-common/pg_service.conf");
    if sys.exists() {
        return Some(sys);
    }

    None
}

/// Parse a `pg_service.conf` file and return all sections as a map of
/// `service_name → { key → value }`.
///
/// Format rules:
/// - `[section_name]` starts a new service block.
/// - `key=value` lines (optional whitespace around `=`) set parameters.
/// - Lines starting with `#` are comments.
/// - Blank lines are ignored.
/// - Only keys listed in `SERVICE_VALID_KEYS` are returned; others are
///   silently ignored.
pub fn parse_service_file(contents: &str) -> HashMap<String, HashMap<String, String>> {
    let mut result: HashMap<String, HashMap<String, String>> = HashMap::new();
    let mut current_section: Option<String> = None;

    for line in contents.lines() {
        let line = line.trim();

        // Skip blank lines and comments.
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        // Section header.
        if let Some(inner) = line.strip_prefix('[') {
            if let Some(name) = inner.strip_suffix(']') {
                let name = name.trim().to_owned();
                result.entry(name.clone()).or_default();
                current_section = Some(name);
            }
            continue;
        }

        // Key=value pair.
        if let Some(ref section) = current_section {
            if let Some((k, v)) = line.split_once('=') {
                let key = k.trim();
                let value = v.trim().to_owned();
                if SERVICE_VALID_KEYS.contains(&key) {
                    result
                        .entry(section.clone())
                        .or_default()
                        .insert(key.to_owned(), value);
                }
            }
        }
    }

    result
}

/// Look up a named service in the service file(s) and return its key-value
/// parameters.
///
/// Returns an empty map if no service file is found, or if the named service
/// does not exist in the file (matching psql behaviour — no error is raised
/// for a missing service file, only for a service file that exists but does
/// not contain the requested service).
fn resolve_service(name: &str) -> Result<HashMap<String, String>, ConnectionError> {
    let Some(path) = service_file_path() else {
        return Ok(HashMap::new());
    };

    let contents = std::fs::read_to_string(&path).map_err(|e| {
        ConnectionError::ServiceFileError(format!(
            "cannot read service file \"{}\": {e}",
            path.display()
        ))
    })?;

    let all = parse_service_file(&contents);

    match all.into_iter().find(|(k, _)| k == name) {
        Some((_, params)) => Ok(params),
        None => Err(ConnectionError::ServiceFileError(format!(
            "definition of service \"{name}\" not found"
        ))),
    }
}

// ---------------------------------------------------------------------------
// .pgpass support
// ---------------------------------------------------------------------------

/// Look up a password in the `.pgpass` file.
pub fn pgpass_lookup(params: &ConnParams) -> Result<Option<String>, ConnectionError> {
    let path = pgpass_path();
    let Some(path) = path else {
        return Ok(None);
    };

    if !path.exists() {
        return Ok(None);
    }

    // On Unix, check permissions.
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let meta = std::fs::metadata(&path).map_err(|e| {
            ConnectionError::PgpassError(format!("cannot stat {}: {e}", path.display()))
        })?;
        let mode = meta.mode() & 0o777;
        if mode & 0o077 != 0 {
            eprintln!(
                "WARNING: password file \"{}\" has group or world access; \
                 permissions should be u=rw (0600) or less",
                path.display()
            );
            return Ok(None);
        }
    }

    let contents = std::fs::read_to_string(&path).map_err(|e| {
        ConnectionError::PgpassError(format!("cannot read {}: {e}", path.display()))
    })?;

    Ok(pgpass_find_match(
        &contents,
        &params.host,
        params.port,
        &params.dbname,
        &params.user,
    ))
}

/// Return the path to the pgpass file.
fn pgpass_path() -> Option<PathBuf> {
    if let Ok(p) = env::var("PGPASSFILE") {
        return Some(PathBuf::from(p));
    }

    #[cfg(unix)]
    {
        dirs::home_dir().map(|h| h.join(".pgpass"))
    }

    #[cfg(windows)]
    {
        env::var("APPDATA")
            .ok()
            .map(|d| PathBuf::from(d).join("postgresql").join("pgpass.conf"))
    }

    #[cfg(not(any(unix, windows)))]
    {
        None
    }
}

/// Parse pgpass file contents and find the first matching entry.
fn pgpass_find_match(
    contents: &str,
    host: &str,
    port: u16,
    dbname: &str,
    user: &str,
) -> Option<String> {
    let port_str = port.to_string();

    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let fields = pgpass_split_line(line);
        if fields.len() < 5 {
            continue;
        }

        if pgpass_field_matches(&fields[0], host)
            && pgpass_field_matches(&fields[1], &port_str)
            && pgpass_field_matches(&fields[2], dbname)
            && pgpass_field_matches(&fields[3], user)
        {
            return Some(fields[4].clone());
        }
    }
    None
}

/// Split a pgpass line on unescaped colons.
fn pgpass_split_line(line: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut current = String::new();
    let mut chars = line.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '\\' {
            if let Some(&next) = chars.peek() {
                if next == ':' || next == '\\' {
                    current.push(next);
                    chars.next();
                    continue;
                }
            }
            current.push(ch);
        } else if ch == ':' {
            fields.push(std::mem::take(&mut current));
        } else {
            current.push(ch);
        }
    }
    fields.push(current);
    fields
}

/// Check if a pgpass field matches a value (`*` is wildcard).
fn pgpass_field_matches(field: &str, value: &str) -> bool {
    field == "*" || field == value
}

// ---------------------------------------------------------------------------
// Password resolution
// ---------------------------------------------------------------------------

/// Resolve the password, trying pgpass and prompting if needed.
///
/// The `server_requested_auth` flag should be `true` when the server
/// has demanded a password and we don't have one yet. On the initial
/// resolve (before connect) we set it to `false`.
pub fn resolve_password(
    params: &mut ConnParams,
    force_prompt: bool,
    no_password: bool,
    server_requested_auth: bool,
) -> Result<(), ConnectionError> {
    // Already have a password from URI or PGPASSWORD.
    if params.password.is_some() && !force_prompt {
        return Ok(());
    }

    // Try .pgpass.
    if params.password.is_none() {
        if let Some(pw) = pgpass_lookup(params)? {
            params.password = Some(pw);
            if !force_prompt {
                return Ok(());
            }
        }
    }

    // Interactive prompt (-W or server requested).
    if force_prompt || (server_requested_auth && !no_password) {
        let prompt = format!("Password for user {}: ", params.user);
        match rpassword::prompt_password(&prompt) {
            Ok(pw) => {
                params.password = Some(pw);
            }
            Err(e) => {
                return Err(ConnectionError::AuthenticationFailed {
                    user: params.user.clone(),
                    reason: format!("could not read password: {e}"),
                });
            }
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// TLS configuration
// ---------------------------------------------------------------------------

/// Build a default `rustls` `ClientConfig` using Mozilla/webpki root certs.
///
/// This is used for `sslmode=prefer`, `sslmode=require`, and as the basis
/// for `sslmode=verify-ca` / `sslmode=verify-full` when no custom CA is set.
fn make_tls_config_default() -> ClientConfig {
    let root_store: rustls::RootCertStore =
        webpki_roots::TLS_SERVER_ROOTS.iter().cloned().collect();

    ClientConfig::builder()
        .with_root_certificates(root_store)
        .with_no_client_auth()
}

/// Build a `ClientConfig` for `sslmode=require`.
///
/// Encrypts the connection but performs no certificate or hostname verification.
/// Supports optional client certificates (mutual TLS).
fn make_tls_config_require(params: &ConnParams) -> Result<ClientConfig, ConnectionError> {
    let verifier = Arc::new(NoVerifier::new());
    let builder = ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(verifier);

    match (&params.ssl_cert, &params.ssl_key) {
        (Some(cert), Some(key)) => {
            let (certs, private_key) = load_client_cert_and_key(cert, key)?;
            Ok(builder
                .with_client_auth_cert(certs, private_key)
                .map_err(|e| {
                    ConnectionError::SslClientCertError(format!("invalid client cert/key: {e}"))
                })?)
        }
        (Some(_), None) | (None, Some(_)) => {
            eprintln!(
                "WARNING: both sslcert and sslkey must be set for \
                 client certificate authentication; ignoring"
            );
            Ok(builder.with_no_client_auth())
        }
        (None, None) => Ok(builder.with_no_client_auth()),
    }
}

/// Load PEM certificates from `path` into a `RootCertStore`.
fn load_root_cert_store(path: &str) -> Result<rustls::RootCertStore, ConnectionError> {
    let pem = std::fs::read(path)
        .map_err(|e| ConnectionError::SslRootCertError(format!("cannot read {path}: {e}")))?;

    let mut store = rustls::RootCertStore::empty();
    let certs: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut pem.as_slice())
        .filter_map(Result::ok)
        .map(CertificateDer::into_owned)
        .collect();

    if certs.is_empty() {
        return Err(ConnectionError::SslRootCertError(format!(
            "no PEM certificates found in {path}"
        )));
    }

    for cert in certs {
        store.add(cert).map_err(|e| {
            ConnectionError::SslRootCertError(format!("invalid certificate in {path}: {e}"))
        })?;
    }

    Ok(store)
}

/// Load all PEM certificates from `path` as raw `CertificateDer` bytes.
///
/// Used to supply local certificates as intermediates when verifying a chain
/// that the server did not send in full (issue #712).  Returns an empty vec
/// if the path is `None` or the file cannot be read.
fn load_certs_as_intermediates(path: Option<&str>) -> Vec<CertificateDer<'static>> {
    let Some(p) = path else { return vec![] };
    let Ok(pem) = std::fs::read(p) else {
        return vec![];
    };
    rustls_pemfile::certs(&mut pem.as_slice())
        .filter_map(Result::ok)
        .map(CertificateDer::into_owned)
        .collect()
}

/// Load a client certificate and private key from PEM files.
///
/// Returns `(cert_chain, private_key)` suitable for passing to
/// `ClientConfig::with_client_auth_cert()`.
fn load_client_cert_and_key(
    cert_path: &str,
    key_path: &str,
) -> Result<
    (
        Vec<CertificateDer<'static>>,
        rustls::pki_types::PrivateKeyDer<'static>,
    ),
    ConnectionError,
> {
    let cert_err = |e: String| ConnectionError::SslClientCertError(e);
    let key_err = |e: String| ConnectionError::SslClientCertError(e);

    let cert_pem = std::fs::read(cert_path)
        .map_err(|e| cert_err(format!("cannot read cert {cert_path}: {e}")))?;
    let certs: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut cert_pem.as_slice())
        .filter_map(Result::ok)
        .map(CertificateDer::into_owned)
        .collect();
    if certs.is_empty() {
        return Err(cert_err(format!(
            "no PEM certificates found in {cert_path}"
        )));
    }

    let key_pem =
        std::fs::read(key_path).map_err(|e| key_err(format!("cannot read key {key_path}: {e}")))?;
    let key = rustls_pemfile::private_key(&mut key_pem.as_slice())
        .map_err(|e| key_err(format!("cannot parse key {key_path}: {e}")))?
        .ok_or_else(|| key_err(format!("no private key found in {key_path}")))?;

    Ok((certs, key))
}

/// Build a `ClientConfig` for `sslmode=verify-ca`.
///
/// The certificate chain is verified against the CA bundle but the server
/// hostname is NOT checked — matching psql `sslmode=verify-ca` semantics.
fn make_tls_config_verify_ca(params: &ConnParams) -> Result<ClientConfig, ConnectionError> {
    let root_store = match &params.ssl_root_cert {
        Some(path) => load_root_cert_store(path)?,
        None => webpki_roots::TLS_SERVER_ROOTS.iter().cloned().collect(),
    };

    // Load certs from the same bundle as potential intermediates.  PostgreSQL
    // sends only the leaf cert by default; these extras allow NoCnVerifier to
    // complete the chain when the server omits intermediate certs (issue #712).
    let extra = load_certs_as_intermediates(params.ssl_root_cert.as_deref());

    // Use a custom verifier that checks the certificate chain against our CA
    // store but does NOT verify the server hostname.
    let verifier = Arc::new(NoCnVerifier::new(root_store, extra));
    let builder = ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(verifier);

    match (&params.ssl_cert, &params.ssl_key) {
        (Some(cert), Some(key)) => {
            let (certs, private_key) = load_client_cert_and_key(cert, key)?;
            Ok(builder
                .with_client_auth_cert(certs, private_key)
                .map_err(|e| {
                    ConnectionError::SslClientCertError(format!("invalid client cert/key: {e}"))
                })?)
        }
        (Some(_), None) | (None, Some(_)) => {
            eprintln!(
                "WARNING: both sslcert and sslkey must be set for \
                 client certificate authentication; ignoring"
            );
            Ok(builder.with_no_client_auth())
        }
        (None, None) => Ok(builder.with_no_client_auth()),
    }
}

/// Build a `ClientConfig` for `sslmode=verify-full`.
///
/// Performs full chain + hostname validation.  Uses `FullVerifier` so that
/// when the server sends only the leaf cert (`PostgreSQL`'s default), the certs
/// in `sslrootcert` are tried as intermediates before giving up (issue #712).
fn make_tls_config_verify_full(params: &ConnParams) -> Result<ClientConfig, ConnectionError> {
    let root_store = match &params.ssl_root_cert {
        Some(path) => load_root_cert_store(path)?,
        None => webpki_roots::TLS_SERVER_ROOTS.iter().cloned().collect(),
    };

    let extra = load_certs_as_intermediates(params.ssl_root_cert.as_deref());
    let verifier = Arc::new(FullVerifier::new(root_store, extra));
    let builder = ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(verifier);

    match (&params.ssl_cert, &params.ssl_key) {
        (Some(cert), Some(key)) => {
            let (certs, private_key) = load_client_cert_and_key(cert, key)?;
            Ok(builder
                .with_client_auth_cert(certs, private_key)
                .map_err(|e| {
                    ConnectionError::SslClientCertError(format!("invalid client cert/key: {e}"))
                })?)
        }
        (Some(_), None) | (None, Some(_)) => {
            eprintln!(
                "WARNING: both sslcert and sslkey must be set for \
                 client certificate authentication; ignoring"
            );
            Ok(builder.with_no_client_auth())
        }
        (None, None) => Ok(builder.with_no_client_auth()),
    }
}

// ---------------------------------------------------------------------------
// Custom certificate verifier: require (no verification at all)
// ---------------------------------------------------------------------------

/// A `ServerCertVerifier` that accepts any server certificate without
/// performing any chain or hostname validation.
///
/// This implements `sslmode=require` semantics: the connection is encrypted
/// but the server identity is not verified.  psql behaves identically.
#[derive(Debug)]
struct NoVerifier {
    provider: Arc<rustls::crypto::CryptoProvider>,
}

impl NoVerifier {
    fn new() -> Self {
        Self {
            provider: Arc::new(rustls::crypto::ring::default_provider()),
        }
    }
}

impl ServerCertVerifier for NoVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, RustlsError> {
        // sslmode=require: encrypt only, no cert verification.
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, RustlsError> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, RustlsError> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

// ---------------------------------------------------------------------------
// Custom certificate verifier: verify-full with chain completion
// ---------------------------------------------------------------------------

/// A `ServerCertVerifier` that performs full chain + hostname validation but
/// also retries with extra intermediates from `sslrootcert` when the server
/// sends only the leaf cert (issue #712).
///
/// This implements `sslmode=verify-full` semantics matching psql/OpenSSL.
#[derive(Debug)]
struct FullVerifier {
    roots: rustls::RootCertStore,
    extra_intermediates: Vec<CertificateDer<'static>>,
    provider: Arc<rustls::crypto::CryptoProvider>,
}

impl FullVerifier {
    fn new(
        roots: rustls::RootCertStore,
        extra_intermediates: Vec<CertificateDer<'static>>,
    ) -> Self {
        Self {
            roots,
            extra_intermediates,
            provider: Arc::new(rustls::crypto::ring::default_provider()),
        }
    }

    fn verify_chain(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, RustlsError> {
        let verifier = rustls::client::WebPkiServerVerifier::builder_with_provider(
            Arc::new(self.roots.clone()),
            Arc::clone(&self.provider),
        )
        .build()
        .map_err(|e| RustlsError::General(format!("cannot build WebPkiServerVerifier: {e}")))?;

        verifier.verify_server_cert(end_entity, intermediates, server_name, ocsp_response, now)
    }
}

impl ServerCertVerifier for FullVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, RustlsError> {
        // First attempt: what the server sent.
        match self.verify_chain(end_entity, intermediates, server_name, ocsp_response, now) {
            Ok(ok) => return Ok(ok),
            Err(RustlsError::InvalidCertificate(rustls::CertificateError::UnknownIssuer))
                if !self.extra_intermediates.is_empty() => {}
            Err(e) => return Err(e),
        }

        // Second attempt: augment with extras from sslrootcert (issue #712).
        let mut augmented: Vec<CertificateDer<'_>> = self
            .extra_intermediates
            .iter()
            .map(|c| c.as_ref().into())
            .collect();
        augmented.extend_from_slice(intermediates);

        self.verify_chain(end_entity, &augmented, server_name, ocsp_response, now)
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, RustlsError> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, RustlsError> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

// ---------------------------------------------------------------------------
// Custom certificate verifier: verify-ca (chain only, no hostname check)
// ---------------------------------------------------------------------------

/// A `ServerCertVerifier` that validates the certificate chain against a
/// given CA store but does NOT verify the server hostname.
///
/// This implements `sslmode=verify-ca` semantics.
///
/// ## Chain completion (issue #712)
///
/// rustls requires the full certificate chain to be present in the TLS
/// handshake.  `PostgreSQL` by default sends only the leaf certificate.
/// OpenSSL (used by psql/libpq) can complete the chain from the local trust
/// store; rustls cannot.
///
/// To bridge this gap, `NoCnVerifier` accepts an optional list of
/// `extra_intermediates` — certificates loaded from the `sslrootcert` bundle.
/// When the chain cannot be verified with the certs the server sent, it
/// retries with those extra intermediates prepended.  This allows the common
/// case where `sslrootcert` contains both the CA root and any intermediate
/// CA certs.
#[derive(Debug)]
struct NoCnVerifier {
    roots: rustls::RootCertStore,
    /// Certificates from the sslrootcert file, used as fallback intermediates.
    extra_intermediates: Vec<CertificateDer<'static>>,
    provider: Arc<rustls::crypto::CryptoProvider>,
}

impl NoCnVerifier {
    fn new(
        roots: rustls::RootCertStore,
        extra_intermediates: Vec<CertificateDer<'static>>,
    ) -> Self {
        Self {
            roots,
            extra_intermediates,
            provider: Arc::new(rustls::crypto::ring::default_provider()),
        }
    }

    fn verify_chain(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, RustlsError> {
        // Use a dummy hostname so WebPkiServerVerifier's name check triggers
        // a known, ignorable error (NotValidForName) rather than a real one.
        let dummy_name = ServerName::try_from("dummy.invalid")
            .map_err(|_| RustlsError::General("invalid dummy hostname".into()))?;

        let verifier = rustls::client::WebPkiServerVerifier::builder_with_provider(
            Arc::new(self.roots.clone()),
            Arc::clone(&self.provider),
        )
        .build()
        .map_err(|e| RustlsError::General(format!("cannot build WebPkiServerVerifier: {e}")))?;

        match verifier.verify_server_cert(
            end_entity,
            intermediates,
            &dummy_name,
            ocsp_response,
            now,
        ) {
            Ok(ok) => Ok(ok),
            // Hostname mismatch on the dummy name is expected — treat as success.
            // rustls 0.23 has two variants for name mismatch: the legacy
            // `NotValidForName` and the richer `NotValidForNameContext` (which
            // includes the expected name and presented SANs).  webpki 0.103+
            // always returns the context variant, so we must catch both.
            Err(RustlsError::InvalidCertificate(
                rustls::CertificateError::NotValidForName
                | rustls::CertificateError::NotValidForNameContext { .. },
            )) => Ok(ServerCertVerified::assertion()),
            Err(e) => Err(e),
        }
    }
}

impl ServerCertVerifier for NoCnVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, RustlsError> {
        // First attempt: use only what the server sent.
        match self.verify_chain(end_entity, intermediates, ocsp_response, now) {
            Ok(ok) => return Ok(ok),
            Err(RustlsError::InvalidCertificate(rustls::CertificateError::UnknownIssuer))
                if !self.extra_intermediates.is_empty() =>
            {
                // Chain is incomplete — the server sent only the leaf cert
                // (PostgreSQL's default behaviour).  Retry with the certs from
                // the sslrootcert bundle prepended as intermediates.  This
                // matches what OpenSSL/psql does automatically (issue #712).
            }
            Err(e) => return Err(e),
        }

        // Second attempt: augment the server's intermediates with our extras.
        let mut augmented: Vec<CertificateDer<'_>> = self
            .extra_intermediates
            .iter()
            .map(|c| c.as_ref().into())
            .collect();
        augmented.extend_from_slice(intermediates);

        self.verify_chain(end_entity, &augmented, ocsp_response, now)
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, RustlsError> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, RustlsError> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

// ---------------------------------------------------------------------------
// Capturing TLS connector
// ---------------------------------------------------------------------------

/// Shared slot written by [`CapturingTlsStream`] when the TLS handshake
/// completes.  The [`connect`] function reads from this after
/// `pg_config.connect()` resolves.
type TlsInfoSlot = Arc<Mutex<Option<TlsInfo>>>;

/// A [`MakeTlsConnect`] that wraps `tokio-rustls` and captures the negotiated
/// TLS protocol version and cipher suite into a shared [`TlsInfoSlot`].
struct CapturingMakeConnect {
    connector: TlsConnector,
    slot: TlsInfoSlot,
}

impl CapturingMakeConnect {
    fn new(config: ClientConfig, slot: TlsInfoSlot) -> Self {
        Self {
            connector: TlsConnector::from(Arc::new(config)),
            slot,
        }
    }
}

impl<S> MakeTlsConnect<S> for CapturingMakeConnect
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    type Stream = CapturingTlsStream<S>;
    type TlsConnect = CapturingConnect<S>;
    type Error = rustls::pki_types::InvalidDnsNameError;

    fn make_tls_connect(&mut self, hostname: &str) -> Result<Self::TlsConnect, Self::Error> {
        let server_name = ServerName::try_from(hostname)?.to_owned();
        Ok(CapturingConnect {
            server_name,
            connector: self.connector.clone(),
            slot: Arc::clone(&self.slot),
            _marker: std::marker::PhantomData,
        })
    }
}

/// The [`TlsConnect`] returned by [`CapturingMakeConnect`].
struct CapturingConnect<S> {
    server_name: ServerName<'static>,
    connector: TlsConnector,
    slot: TlsInfoSlot,
    _marker: std::marker::PhantomData<S>,
}

impl<S> TlsConnect<S> for CapturingConnect<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    type Stream = CapturingTlsStream<S>;
    type Error = io::Error;
    type Future = Pin<Box<dyn Future<Output = io::Result<Self::Stream>> + Send>>;

    fn connect(self, stream: S) -> Self::Future {
        let Self {
            server_name,
            connector,
            slot,
            ..
        } = self;

        Box::pin(async move {
            let tls_stream = connector.connect(server_name, stream).await?;

            // After the handshake the session info is available.
            let (_, session) = tls_stream.get_ref();
            let info = TlsInfo {
                protocol: session
                    .protocol_version()
                    .map_or_else(|| "TLS".to_owned(), protocol_version_str),
                cipher: session
                    .negotiated_cipher_suite()
                    .map_or_else(|| "unknown".to_owned(), |cs| cipher_suite_str(cs.suite())),
            };
            *slot.lock().unwrap() = Some(info);

            Ok(CapturingTlsStream(Box::pin(tls_stream)))
        })
    }
}

/// Thin wrapper around `tokio_rustls::client::TlsStream` that implements
/// the `tokio_postgres::tls::TlsStream` trait.
struct CapturingTlsStream<S>(Pin<Box<TlsStream<S>>>);

impl<S> tokio_postgres::tls::TlsStream for CapturingTlsStream<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    fn channel_binding(&self) -> ChannelBinding {
        use x509_certificate::{DigestAlgorithm, SignatureAlgorithm, X509Certificate};
        use DigestAlgorithm::{Sha1, Sha256, Sha384, Sha512};
        use SignatureAlgorithm::{
            EcdsaSha256, EcdsaSha384, Ed25519, NoSignature, RsaSha1, RsaSha256, RsaSha384,
            RsaSha512,
        };

        let (_, session) = self.0.get_ref();
        match session.peer_certificates() {
            Some(certs) if !certs.is_empty() => X509Certificate::from_der(&certs[0])
                .ok()
                .and_then(|cert| cert.signature_algorithm())
                .map_or_else(ChannelBinding::none, |algorithm| {
                    let alg = match algorithm {
                        RsaSha1 | RsaSha256 | EcdsaSha256 => &ring::digest::SHA256,
                        RsaSha384 | EcdsaSha384 => &ring::digest::SHA384,
                        RsaSha512 | Ed25519 => &ring::digest::SHA512,
                        NoSignature(algo) => match algo {
                            Sha1 | Sha256 => &ring::digest::SHA256,
                            Sha384 => &ring::digest::SHA384,
                            Sha512 => &ring::digest::SHA512,
                        },
                    };
                    let hash = ring::digest::digest(alg, certs[0].as_ref());
                    ChannelBinding::tls_server_end_point(hash.as_ref().into())
                }),
            _ => ChannelBinding::none(),
        }
    }
}

impl<S> AsyncRead for CapturingTlsStream<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        self.0.as_mut().poll_read(cx, buf)
    }
}

impl<S> AsyncWrite for CapturingTlsStream<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        self.0.as_mut().poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.0.as_mut().poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.0.as_mut().poll_shutdown(cx)
    }
}

// ---------------------------------------------------------------------------
// Connect
// ---------------------------------------------------------------------------

/// Establish a connection to Postgres and return both the `Client` and the
/// fully-resolved `ConnParams` that were used.
///
/// When `params.hosts` contains multiple entries, each host is tried in order
/// until one accepts a connection that satisfies `params.target_session_attrs`.
/// For `prefer-standby` the entire list is tried for standbys first; if none
/// qualify, the list is retried accepting any host.
#[allow(clippy::too_many_lines)]
pub async fn connect(
    mut params: ConnParams,
    opts: &CliConnOpts,
) -> Result<(Client, ConnParams), ConnectionError> {
    // Resolve password (pre-connect: may prompt if -W).
    resolve_password(&mut params, opts.force_password, opts.no_password, false)?;

    let hosts = params.hosts.clone();
    let tsa = params.target_session_attrs;

    // For prefer-standby we may need two passes.
    // Use a fixed-size array to avoid heap allocation; track length separately.
    let passes_buf: [TargetSessionAttrs; 2];
    let passes: &[TargetSessionAttrs] = match tsa {
        TargetSessionAttrs::PreferStandby => {
            passes_buf = [TargetSessionAttrs::Standby, TargetSessionAttrs::Any];
            &passes_buf
        }
        other => {
            passes_buf = [other, other]; // second slot unused
            &passes_buf[..1]
        }
    };

    let mut last_err: Option<ConnectionError> = None;

    'outer: for &effective_tsa in passes {
        for (host, port) in &hosts {
            // Build a fresh tokio-postgres config for this candidate host.
            let mut pg_config = tokio_postgres::Config::new();
            pg_config
                .host(host)
                .port(*port)
                .user(&params.user)
                .dbname(&params.dbname)
                .application_name(&params.application_name);

            if let Some(ref pw) = params.password {
                pg_config.password(pw);
            }
            if let Some(timeout) = params.connect_timeout {
                pg_config.connect_timeout(std::time::Duration::from_secs(timeout));
            }
            if let Some(ref opts_str) = params.options {
                pg_config.options(opts_str);
            }

            // Temporarily set the candidate host in params so the TLS
            // helper functions (verify-full) use the right hostname.
            params.host = host.clone();
            params.port = *port;

            let result = connect_one(&pg_config, &params).await;
            let (client, tls_info) = match result {
                Ok(pair) => pair,
                Err(e) => {
                    // When the kernel does not support IPv6 (EAFNOSUPPORT /
                    // os error 97) tokio may fail without falling back to the
                    // IPv4 address that the same hostname also resolves to.
                    // Retry with an explicit IPv4 address so that connecting
                    // to "localhost" works even when IPv6 is not available.
                    if is_af_not_supported_err(&e)
                        && !host.starts_with('/')
                        && !is_numeric_addr(host)
                    {
                        let addr_str = format!("{host}:{port}");
                        let ipv4 = std::net::ToSocketAddrs::to_socket_addrs(&addr_str.as_str())
                            .ok()
                            .and_then(|mut it| {
                                it.find(std::net::SocketAddr::is_ipv4)
                                    .map(|a| a.ip().to_string())
                            });

                        if let Some(ip4) = ipv4 {
                            let mut pg4 = pg_config.clone();
                            pg4.host(&ip4);
                            // Keep params.host as the original hostname so
                            // that TLS hostname verification (verify-full)
                            // still checks the certificate against the DNS
                            // name, not the resolved IP literal.  Only the
                            // dial address (pg4) changes to IPv4.
                            match connect_one(&pg4, &params).await {
                                Ok(pair) => {
                                    // Record the resolved IP for \conninfo
                                    // display without overwriting the hostname.
                                    params.resolved_addr = Some(ip4);
                                    pair
                                }
                                Err(e2) => {
                                    last_err = Some(e2);
                                    continue;
                                }
                            }
                        } else {
                            last_err = Some(e);
                            continue;
                        }
                    } else {
                        last_err = Some(e);
                        continue;
                    }
                }
            };

            // Verify session attributes if needed.
            if effective_tsa != TargetSessionAttrs::Any {
                match check_session_attrs(&client, effective_tsa).await {
                    Ok(true) => {}
                    Ok(false) => {
                        // This host doesn't match; drop the client and try next.
                        last_err = Some(ConnectionError::NoSuitableHost {
                            tried: hosts.len(),
                            attrs: tsa.to_string(),
                        });
                        continue;
                    }
                    Err(e) => {
                        last_err = Some(e);
                        continue;
                    }
                }
            }

            // Successful connection on this host.
            params.tls_info = tls_info;
            // host/port already updated above.

            // Resolve hostname → IP for \conninfo display.
            if !params.host.starts_with('/') && !is_numeric_addr(&params.host) {
                let addr_str = format!("{}:{}", params.host, params.port);
                if let Ok(mut addrs) = std::net::ToSocketAddrs::to_socket_addrs(&addr_str.as_str())
                {
                    if let Some(addr) = addrs.next() {
                        params.resolved_addr = Some(addr.ip().to_string());
                    }
                }
            }

            return Ok((client, params));
        }

        // If we exhausted all hosts on the standby pass and none qualified,
        // the outer loop will retry with Any.  If this was already the Any
        // pass (or single-pass mode), we break and fall through to the error.
        if effective_tsa == TargetSessionAttrs::Any || passes.len() == 1 {
            break 'outer;
        }
    }

    Err(last_err.unwrap_or(ConnectionError::NoSuitableHost {
        tried: hosts.len(),
        attrs: tsa.to_string(),
    }))
}

/// Attempt a single connection (one host) respecting `params.sslmode`.
///
/// Returns the connected `Client` together with the captured [`TlsInfo`] when
/// TLS was used, or `None` for a plain connection.
async fn connect_one(
    pg_config: &tokio_postgres::Config,
    params: &ConnParams,
) -> Result<(Client, Option<TlsInfo>), ConnectionError> {
    let result = match params.sslmode {
        SslMode::Disable => (connect_plain(pg_config, params).await?, None),

        // sslmode=allow: try plain first; if the server rejects it and
        // demands SSL, retry with TLS.
        SslMode::Allow => match connect_plain(pg_config, params).await {
            Ok(c) => (c, None),
            Err(ConnectionError::SslRequired) => {
                let (c, info) = connect_tls_default(pg_config, params).await?;
                (c, Some(info))
            }
            Err(e) => return Err(e),
        },

        SslMode::Prefer => {
            // sslmode=prefer: try TLS without certificate verification, then
            // fall back to plaintext only if the server does not support TLS.
            // This matches psql / libpq semantics: prefer encrypts when
            // possible but does not reject self-signed certificates.
            //
            // Timeout handling (issue #723): when connect_timeout is set,
            // tokio-postgres applies it per attempt.  Without special handling,
            // sslmode=prefer would try TLS (consuming the full timeout), then
            // fall back to plain (consuming the full timeout again), making the
            // total elapsed time ~2× the configured value.
            //
            // Fix: if the TLS probe itself times out the host is unreachable,
            // so we propagate the timeout error immediately without falling
            // back.  Only non-timeout TLS failures (server rejects SSL,
            // handshake error, etc.) cause the plain fallback.
            let tls_cfg = make_tls_config_require(params)?;
            match connect_tls_with_config(pg_config, params, tls_cfg).await {
                Ok((c, info)) => (c, Some(info)),
                Err(ref e) if is_timeout_error(e) => {
                    // Timed out during TLS probe — propagate immediately.
                    // Falling back to plain would just time out again.
                    return Err(ConnectionError::ConnectionFailed {
                        host: params.host.clone(),
                        port: params.port,
                        reason: format!(
                            "connection timed out (PGCONNECT_TIMEOUT={timeout}s)",
                            timeout = params.connect_timeout.unwrap_or(0)
                        ),
                    });
                }
                Err(_) => {
                    // Non-timeout failure (server does not support TLS, SSL
                    // handshake refused, etc.) — fall back to plaintext.
                    // No warning is shown — matching psql's default behaviour.
                    (connect_plain(pg_config, params).await?, None)
                }
            }
        }

        SslMode::Require => {
            let mut cfg = pg_config.clone();
            cfg.ssl_mode(TokioSslMode::Require);
            let tls_cfg = make_tls_config_require(params)?;
            let (c, info) = connect_tls_with_config(&cfg, params, tls_cfg).await?;
            (c, Some(info))
        }

        SslMode::VerifyCa => {
            let mut cfg = pg_config.clone();
            cfg.ssl_mode(TokioSslMode::Require);
            let tls_cfg = make_tls_config_verify_ca(params)?;
            let (c, info) = connect_tls_with_config(&cfg, params, tls_cfg).await?;
            (c, Some(info))
        }

        SslMode::VerifyFull => {
            let mut cfg = pg_config.clone();
            cfg.ssl_mode(TokioSslMode::Require);
            let tls_cfg = make_tls_config_verify_full(params)?;
            let (c, info) = connect_tls_with_config(&cfg, params, tls_cfg).await?;
            (c, Some(info))
        }
    };
    Ok(result)
}

/// Verify that a newly-established `client` satisfies `tsa`.
///
/// Returns `Ok(true)` when the host qualifies, `Ok(false)` when it does not.
/// Errors indicate a query failure, not a mismatch.
async fn check_session_attrs(
    client: &Client,
    tsa: TargetSessionAttrs,
) -> Result<bool, ConnectionError> {
    match tsa {
        TargetSessionAttrs::Any | TargetSessionAttrs::PreferStandby => Ok(true),

        TargetSessionAttrs::ReadWrite | TargetSessionAttrs::Primary => {
            // read-write / primary: transaction_read_only must be 'off'.
            let row = client
                .query_one("show transaction_read_only", &[])
                .await
                .map_err(|e| ConnectionError::ConnectionFailed {
                    host: String::new(),
                    port: 0,
                    reason: format!("could not check transaction_read_only: {e}"),
                })?;
            let val: &str = row.get(0);
            Ok(val.trim() == "off")
        }

        TargetSessionAttrs::ReadOnly => {
            // read-only: transaction_read_only must be 'on'.
            let row = client
                .query_one("show transaction_read_only", &[])
                .await
                .map_err(|e| ConnectionError::ConnectionFailed {
                    host: String::new(),
                    port: 0,
                    reason: format!("could not check transaction_read_only: {e}"),
                })?;
            let val: &str = row.get(0);
            Ok(val.trim() == "on")
        }

        TargetSessionAttrs::Standby => {
            // standby: pg_is_in_recovery() must return true.
            let row = client
                .query_one("select pg_is_in_recovery()", &[])
                .await
                .map_err(|e| ConnectionError::ConnectionFailed {
                    host: String::new(),
                    port: 0,
                    reason: format!("could not check pg_is_in_recovery(): {e}"),
                })?;
            let in_recovery: bool = row.get(0);
            Ok(in_recovery)
        }
    }
}

/// Drive a tokio-postgres `Connection` to completion, buffering any
/// `PostgreSQL` notice messages (`NOTICE`, `WARNING`, `INFO`, etc.) for
/// later flushing at statement boundaries.
///
/// This replaces the simple `connection.await` pattern so that server-side
/// notices are captured and displayed to the user with colored severity
/// prefixes in deterministic order (after each statement completes), rather
/// than being silently discarded (the tokio-postgres default only logs them
/// via `tracing::info!`) or printed at unpredictable positions due to async
/// task scheduling.
async fn drive_connection<S, T>(
    mut connection: tokio_postgres::Connection<S, T>,
) -> Result<(), tokio_postgres::Error>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    use std::future::poll_fn;
    use tokio_postgres::AsyncMessage;

    loop {
        match poll_fn(|cx| connection.poll_message(cx)).await {
            Some(Ok(AsyncMessage::Notice(notice))) => {
                let formatted = crate::output::format_pg_notice(&notice);
                crate::output::push_notice(formatted);
            }
            Some(Ok(_)) => {
                // Notifications (LISTEN/NOTIFY) — ignore for now.
            }
            Some(Err(e)) => return Err(e),
            None => return Ok(()),
        }
    }
}

/// Connect without TLS.
async fn connect_plain(
    pg_config: &tokio_postgres::Config,
    params: &ConnParams,
) -> Result<Client, ConnectionError> {
    let (client, connection) = pg_config
        .connect(tokio_postgres::NoTls)
        .await
        .map_err(|e| map_connect_error(&e, params))?;

    tokio::spawn(async move {
        if let Err(e) = drive_connection(connection).await {
            eprintln!("rpg: connection error: {e}");
        }
    });

    Ok(client)
}

/// Connect with TLS using the default (webpki) root certificate store.
async fn connect_tls_default(
    pg_config: &tokio_postgres::Config,
    params: &ConnParams,
) -> Result<(Client, TlsInfo), ConnectionError> {
    connect_tls_with_config(pg_config, params, make_tls_config_default()).await
}

/// Connect with TLS using a caller-supplied `ClientConfig`.
///
/// Returns the connected `Client` together with the [`TlsInfo`] captured from
/// the negotiated TLS session (protocol version and cipher suite).
async fn connect_tls_with_config(
    pg_config: &tokio_postgres::Config,
    params: &ConnParams,
    tls_config: ClientConfig,
) -> Result<(Client, TlsInfo), ConnectionError> {
    let slot: TlsInfoSlot = Arc::new(Mutex::new(None));
    let tls = CapturingMakeConnect::new(tls_config, Arc::clone(&slot));

    let (client, connection) = pg_config
        .connect(tls)
        .await
        .map_err(|e| map_connect_error(&e, params))?;

    tokio::spawn(async move {
        if let Err(e) = drive_connection(connection).await {
            eprintln!("rpg: connection error: {e}");
        }
    });

    // The slot is populated synchronously during `connect()` above, before
    // the postgres handshake begins.  It is always `Some` at this point.
    let info = slot.lock().unwrap().take().unwrap_or(TlsInfo {
        protocol: "TLS".to_owned(),
        cipher: "unknown".to_owned(),
    });

    Ok((client, info))
}

/// Returns `true` if `e` is an "address family not supported" OS error
/// (EAFNOSUPPORT / errno 97).  This happens when the kernel has IPv6
/// disabled and tokio resolves a hostname to an IPv6 address first.
fn is_af_not_supported_err(e: &ConnectionError) -> bool {
    match e {
        ConnectionError::ConnectionFailed { reason, .. } => {
            let r = reason.to_lowercase();
            r.contains("address family not supported") || r.contains("os error 97")
        }
        _ => false,
    }
}

/// Returns `true` if `e` represents a connection timeout.
///
/// Used by `sslmode=prefer` to avoid falling back to plaintext when the host
/// is simply unreachable — doing so would double the elapsed time relative to
/// the configured `PGCONNECT_TIMEOUT` (issue #723).
fn is_timeout_error(e: &ConnectionError) -> bool {
    match e {
        ConnectionError::ConnectionFailed { reason, .. } => {
            let r = reason.to_lowercase();
            r.contains("timeout") || r.contains("timed out")
        }
        _ => false,
    }
}

/// Classify an error message string into a `ConnectionError`.
///
/// Extracted from `map_connect_error` so that the classification rules can
/// be unit-tested without a live database connection.
fn classify_connect_error(msg: String, params: &ConnParams) -> ConnectionError {
    if msg.contains("authentication")
        || msg.contains("password")
        || msg.contains("no password")
        || msg.contains("auth")
    {
        return ConnectionError::AuthenticationFailed {
            user: params.user.clone(),
            reason: msg,
        };
    }

    // "SSL connection is required" is the specific server message when the
    // client tries a plain connection but the server demands TLS.
    if msg.contains("SSL connection is required")
        || msg.contains("ssl connection is required")
        || msg.contains("server requires SSL")
    {
        return ConnectionError::SslRequired;
    }

    // tokio-postgres emits this exact string when the server responds with
    // 'N' to the SSLRequest message (i.e., server has no TLS at all).
    if msg.contains("server does not support TLS") {
        return ConnectionError::SslNotSupported;
    }

    // rustls certificate verification failures: chain errors, expired certs,
    // hostname mismatches.  "invalid peer certificate" is the rustls Display
    // prefix for Error::InvalidCertificate.
    if msg.contains("invalid peer certificate") || msg.contains("certificate verify failed") {
        return ConnectionError::SslCertVerificationFailed(msg);
    }

    // General TLS failures: bad cipher, handshake alerts, etc.
    if msg.contains("SSL") || msg.contains("ssl") || msg.contains("TLS") || msg.contains("tls") {
        return ConnectionError::TlsError(msg);
    }

    ConnectionError::ConnectionFailed {
        host: params.host.clone(),
        port: params.port,
        reason: msg,
    }
}

/// Map a `tokio_postgres::Error` into our `ConnectionError`.
///
/// Classification rules:
/// - Authentication keywords → `AuthenticationFailed`
/// - SSL-required signal (server rejects non-TLS when sslmode=disable) →
///   `SslRequired`
/// - Server has no TLS at all (sslmode=require/verify-ca/verify-full) →
///   `SslNotSupported`
/// - Certificate verification failure → `SslCertVerificationFailed`
/// - Other SSL/TLS errors (handshake failure, etc.) → `TlsError`
/// - Everything else → `ConnectionFailed`
fn map_connect_error(e: &tokio_postgres::Error, params: &ConnParams) -> ConnectionError {
    // tokio-postgres wraps TLS errors as `Kind::Tls` with the real cause in
    // the source chain.  The top-level `to_string()` returns "error performing
    // TLS handshake" which is not actionable; walk the chain to find the
    // innermost non-generic message.
    let msg = {
        use std::error::Error as StdError;
        let raw = e.to_string();
        let mut best = raw.clone();
        let mut cause = e.source();
        while let Some(src) = cause {
            let s = src.to_string();
            if !s.is_empty() && s != raw {
                best = s;
            }
            cause = src.source();
        }
        best
    };

    classify_connect_error(msg, params)
}

/// Format a human-friendly connection-success message, matching psql output.
///
/// TCP:    You are connected to database "db" as user "u" on host "h" at port "5432".
/// Socket: You are connected to database "db" as user "u" via socket in "/run/pg" at port "5432".
///
/// When `params.tls_info` is `Some`, the SSL status line is appended:
/// ```text
/// SSL connection (protocol: TLSv1.3, cipher: TLS_AES_256_GCM_SHA384, compression: off)
/// ```
pub fn connection_info(params: &ConnParams) -> String {
    let is_socket = params.host.starts_with('/');
    let connected_line = if is_socket {
        format!(
            "You are connected to database \"{}\" as user \"{}\" \
             via socket in \"{}\" at port \"{}\".",
            params.dbname, params.user, params.host, params.port,
        )
    } else {
        // For TCP connections, include the resolved IP address when it differs
        // from the host string — matching psql's `PQhostaddr()` behaviour.
        match &params.resolved_addr {
            Some(addr) => format!(
                "You are connected to database \"{}\" as user \"{}\" \
                 on host \"{}\" (address \"{}\") at port \"{}\".",
                params.dbname, params.user, params.host, addr, params.port,
            ),
            None => format!(
                "You are connected to database \"{}\" as user \"{}\" \
                 on host \"{}\" at port \"{}\".",
                params.dbname, params.user, params.host, params.port,
            ),
        }
    };
    if let Some(ref info) = params.tls_info {
        format!("{connected_line}\n{}", info.status_line())
    } else {
        connected_line
    }
}

/// Format the `\c` reconnect message, matching psql's output.
///
/// psql always says "You are **now** connected" (with "now") after `\c`,
/// and always prepends a version banner when the server version is known.
///
/// ```text
/// rpg 0.2.0 (...) (server PostgreSQL 17.7)
/// You are now connected to database "mydb" as user "alice" on host "h"
/// at port "5432".
/// ```
///
/// If `server_version` is `None`, the banner is omitted and only the
/// connected line is printed.
///
/// When `new_params.tls_info` is `Some`, the SSL status line is appended after
/// the connected line, matching psql behaviour:
///
/// ```text
/// rpg 0.2.0 (...) (server PostgreSQL 17.7)
/// You are now connected to database "mydb" as user "alice" on host "h"
/// at port "5432".
/// SSL connection (protocol: TLSv1.3, cipher: TLS_AES_256_GCM_SHA384, compression: off)
/// ```
///
/// `client_version` is rpg's own version string (from [`crate::version_string`]).
/// `server_version` is the server's version string from `SHOW server_version`.
/// `new_params` is the newly established connection.
///
/// Returns lines joined by `\n` — the exact number depends on whether a
/// version banner and/or SSL line is needed.
pub fn reconnect_info(
    client_version: &str,
    server_version: Option<&str>,
    new_params: &ConnParams,
) -> String {
    let is_socket = new_params.host.starts_with('/');
    let connected_line = if is_socket {
        format!(
            "You are now connected to database \"{}\" as user \"{}\" \
             via socket in \"{}\" at port \"{}\".",
            new_params.dbname, new_params.user, new_params.host, new_params.port,
        )
    } else {
        format!(
            "You are now connected to database \"{}\" as user \"{}\" \
             on host \"{}\" at port \"{}\".",
            new_params.dbname, new_params.user, new_params.host, new_params.port,
        )
    };

    let ssl_suffix = if let Some(ref info) = new_params.tls_info {
        format!("\n{}", info.status_line())
    } else {
        String::new()
    };

    let banner = if let Some(ver) = server_version {
        format!("{client_version} (server PostgreSQL {ver})\n")
    } else {
        String::new()
    };
    format!("{banner}{connected_line}{ssl_suffix}")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    // -- Parameter resolution: flags > positional > env > defaults ----------

    #[test]
    #[serial]
    fn test_defaults() {
        // Ensure env vars don't interfere.
        let _guard = EnvGuard::new(&[
            "PGHOST",
            "PGPORT",
            "PGDATABASE",
            "PGUSER",
            "PGPASSWORD",
            "PGSSLMODE",
            "PGAPPNAME",
            "PGCONNECT_TIMEOUT",
        ]);

        let opts = CliConnOpts::default();
        let params = resolve_params(&opts).unwrap();

        assert_eq!(params.port, 5432);
        // dbname defaults to user
        assert_eq!(params.dbname, params.user);
        assert_eq!(params.application_name, "rpg");
        assert_eq!(params.sslmode, SslMode::Prefer);
        assert!(params.password.is_none());
    }

    #[test]
    #[serial]
    fn test_cli_flags_override_positionals() {
        let _guard = EnvGuard::new(&["PGHOST", "PGPORT", "PGDATABASE", "PGUSER"]);

        let opts = CliConnOpts {
            host: Some("flag-host".into()),
            port: Some(5555),
            username: Some("flag-user".into()),
            dbname: Some("flag-db".into()),
            dbname_pos: Some("pos-db".into()),
            user_pos: Some("pos-user".into()),
            host_pos: Some("pos-host".into()),
            port_pos: Some("9999".into()),
            ..Default::default()
        };
        let params = resolve_params(&opts).unwrap();

        assert_eq!(params.host, "flag-host");
        assert_eq!(params.port, 5555);
        assert_eq!(params.user, "flag-user");
        assert_eq!(params.dbname, "flag-db");
    }

    #[test]
    #[serial]
    fn test_positionals_used_when_no_flags() {
        let _guard = EnvGuard::new(&["PGHOST", "PGPORT", "PGDATABASE", "PGUSER"]);

        let opts = CliConnOpts {
            dbname_pos: Some("mydb".into()),
            user_pos: Some("myuser".into()),
            host_pos: Some("myhost".into()),
            port_pos: Some("6543".into()),
            ..Default::default()
        };
        let params = resolve_params(&opts).unwrap();

        assert_eq!(params.host, "myhost");
        assert_eq!(params.port, 6543);
        assert_eq!(params.user, "myuser");
        assert_eq!(params.dbname, "mydb");
    }

    #[test]
    #[serial]
    fn test_env_vars_used_as_fallback() {
        let _guard = EnvGuard::new(&[
            "PGHOST",
            "PGPORT",
            "PGDATABASE",
            "PGUSER",
            "PGPASSWORD",
            "PGSSLMODE",
            "PGAPPNAME",
            "PGCONNECT_TIMEOUT",
        ]);

        env::set_var("PGHOST", "env-host");
        env::set_var("PGPORT", "7777");
        env::set_var("PGDATABASE", "env-db");
        env::set_var("PGUSER", "env-user");
        env::set_var("PGPASSWORD", "env-pass");
        env::set_var("PGSSLMODE", "require");
        env::set_var("PGAPPNAME", "env-app");
        env::set_var("PGCONNECT_TIMEOUT", "10");

        let opts = CliConnOpts::default();
        let params = resolve_params(&opts).unwrap();

        assert_eq!(params.host, "env-host");
        assert_eq!(params.port, 7777);
        assert_eq!(params.user, "env-user");
        assert_eq!(params.dbname, "env-db");
        assert_eq!(params.password, Some("env-pass".into()));
        assert_eq!(params.sslmode, SslMode::Require);
        assert_eq!(params.application_name, "env-app");
        assert_eq!(params.connect_timeout, Some(10));
    }

    // -- URI parsing --------------------------------------------------------

    #[test]
    #[serial]
    fn test_parse_uri_full() {
        let _guard = EnvGuard::new(&["PGHOST", "PGPORT", "PGDATABASE", "PGUSER"]);

        let opts = CliConnOpts {
            dbname_pos: Some(
                "postgresql://alice:s3cret@db.example.com:5433/mydb?sslmode=require".into(),
            ),
            ..Default::default()
        };
        let params = resolve_params(&opts).unwrap();

        assert_eq!(params.host, "db.example.com");
        assert_eq!(params.port, 5433);
        assert_eq!(params.user, "alice");
        assert_eq!(params.dbname, "mydb");
        assert_eq!(params.password, Some("s3cret".into()));
        assert_eq!(params.sslmode, SslMode::Require);
    }

    #[test]
    #[serial]
    fn test_parse_uri_minimal() {
        let _guard = EnvGuard::new(&["PGHOST", "PGPORT", "PGDATABASE", "PGUSER"]);

        let opts = CliConnOpts {
            dbname_pos: Some("postgres://localhost/testdb".into()),
            ..Default::default()
        };
        let params = resolve_params(&opts).unwrap();

        assert_eq!(params.host, "localhost");
        assert_eq!(params.dbname, "testdb");
    }

    #[test]
    fn test_parse_uri_percent_encoded() {
        let uri_params = parse_uri("postgresql://us%40er:p%40ss@host/db").unwrap();
        assert_eq!(uri_params.user, Some("us@er".into()));
        assert_eq!(uri_params.password, Some("p@ss".into()));
    }

    #[test]
    fn test_parse_uri_ipv6() {
        let uri_params = parse_uri("postgresql://user@[::1]:5432/db").unwrap();
        assert_eq!(uri_params.host, Some("::1".into()));
        assert_eq!(uri_params.port, Some(5432));
        assert_eq!(uri_params.dbname, Some("db".into()));
    }

    #[test]
    fn test_parse_uri_application_name() {
        let uri_params = parse_uri("postgresql://localhost/db?application_name=myapp").unwrap();
        assert_eq!(uri_params.application_name, Some("myapp".into()));
    }

    #[test]
    fn test_parse_uri_no_host() {
        let uri_params = parse_uri("postgresql:///mydb").unwrap();
        assert!(uri_params.host.is_none());
        assert_eq!(uri_params.dbname, Some("mydb".into()));
    }

    #[test]
    #[serial]
    fn test_parse_uri_connect_timeout() {
        let _guard = EnvGuard::new(&[
            "PGHOST",
            "PGPORT",
            "PGDATABASE",
            "PGUSER",
            "PGCONNECT_TIMEOUT",
        ]);

        let opts = CliConnOpts {
            dbname_pos: Some("postgresql://localhost/db?connect_timeout=5".into()),
            ..Default::default()
        };
        let params = resolve_params(&opts).unwrap();
        assert_eq!(params.connect_timeout, Some(5));
    }

    // -- Key-value conninfo -------------------------------------------------

    #[test]
    #[serial]
    fn test_parse_conninfo_basic() {
        let _guard = EnvGuard::new(&["PGHOST", "PGPORT", "PGDATABASE", "PGUSER"]);

        let opts = CliConnOpts {
            dbname_pos: Some("host=connhost port=6543 dbname=conndb user=connuser".into()),
            ..Default::default()
        };
        let params = resolve_params(&opts).unwrap();

        assert_eq!(params.host, "connhost");
        assert_eq!(params.port, 6543);
        assert_eq!(params.user, "connuser");
        assert_eq!(params.dbname, "conndb");
    }

    #[test]
    fn test_parse_conninfo_quoted_values() {
        let map = parse_conninfo("host='my host' dbname='my db'").unwrap();
        assert_eq!(map.get("host").unwrap(), "my host");
        assert_eq!(map.get("dbname").unwrap(), "my db");
    }

    #[test]
    #[serial]
    fn test_parse_conninfo_sslmode() {
        let _guard = EnvGuard::new(&["PGHOST", "PGPORT", "PGDATABASE", "PGUSER", "PGSSLMODE"]);

        let opts = CliConnOpts {
            dbname_pos: Some("host=h sslmode=require".into()),
            ..Default::default()
        };
        let params = resolve_params(&opts).unwrap();
        assert_eq!(params.sslmode, SslMode::Require);
    }

    // -- CLI --sslmode flag priority ----------------------------------------

    #[test]
    #[serial]
    fn test_cli_sslmode_overrides_env() {
        let _guard = EnvGuard::new(&["PGSSLMODE"]);

        // Even with env set to "disable", CLI flag wins.
        let opts = CliConnOpts {
            sslmode: Some("require".into()),
            ..Default::default()
        };
        let params = resolve_params(&opts).unwrap();
        assert_eq!(params.sslmode, SslMode::Require);
    }

    // -- ConnParams Debug masks password ------------------------------------

    #[test]
    fn test_connparams_debug_masks_password() {
        let params = ConnParams {
            password: Some("supersecret".into()),
            ..ConnParams::default()
        };
        let debug_str = format!("{params:?}");
        assert!(!debug_str.contains("supersecret"));
        assert!(debug_str.contains("***"));
    }

    // -- .pgpass parsing ----------------------------------------------------

    #[test]
    fn test_pgpass_exact_match() {
        let contents = "myhost:5432:mydb:myuser:secret123\n";
        let result = pgpass_find_match(contents, "myhost", 5432, "mydb", "myuser");
        assert_eq!(result, Some("secret123".into()));
    }

    #[test]
    fn test_pgpass_wildcard() {
        let contents = "*:*:*:*:wildcard_pass\n";
        let result = pgpass_find_match(contents, "anyhost", 9999, "anydb", "anyuser");
        assert_eq!(result, Some("wildcard_pass".into()));
    }

    #[test]
    fn test_pgpass_partial_wildcard() {
        let contents = "myhost:*:mydb:*:partial_pass\n";
        let result = pgpass_find_match(contents, "myhost", 5432, "mydb", "anyuser");
        assert_eq!(result, Some("partial_pass".into()));

        let result = pgpass_find_match(contents, "otherhost", 5432, "mydb", "anyuser");
        assert!(result.is_none());
    }

    #[test]
    fn test_pgpass_comments_and_blanks() {
        let contents = "# this is a comment\n\n  \nmyhost:5432:mydb:myuser:pass\n";
        let result = pgpass_find_match(contents, "myhost", 5432, "mydb", "myuser");
        assert_eq!(result, Some("pass".into()));
    }

    #[test]
    fn test_pgpass_first_match_wins() {
        let contents = "myhost:5432:mydb:myuser:first\nmyhost:5432:mydb:myuser:second\n";
        let result = pgpass_find_match(contents, "myhost", 5432, "mydb", "myuser");
        assert_eq!(result, Some("first".into()));
    }

    #[test]
    fn test_pgpass_escaped_colon() {
        let contents = r"myhost:5432:mydb:myuser:pass\:word";
        let result = pgpass_find_match(contents, "myhost", 5432, "mydb", "myuser");
        assert_eq!(result, Some("pass:word".into()));
    }

    #[test]
    fn test_pgpass_no_match() {
        let contents = "otherhost:5432:otherdb:otheruser:pass\n";
        let result = pgpass_find_match(contents, "myhost", 5432, "mydb", "myuser");
        assert!(result.is_none());
    }

    // -- SSL mode parsing ---------------------------------------------------

    #[test]
    fn test_sslmode_parse() {
        assert_eq!(SslMode::parse("disable").unwrap(), SslMode::Disable);
        assert_eq!(SslMode::parse("allow").unwrap(), SslMode::Allow);
        assert_eq!(SslMode::parse("prefer").unwrap(), SslMode::Prefer);
        assert_eq!(SslMode::parse("require").unwrap(), SslMode::Require);
        assert_eq!(SslMode::parse("verify-ca").unwrap(), SslMode::VerifyCa);
        assert_eq!(SslMode::parse("verify-full").unwrap(), SslMode::VerifyFull);
        // Case-insensitive.
        assert_eq!(SslMode::parse("REQUIRE").unwrap(), SslMode::Require);
        assert_eq!(SslMode::parse("Verify-Full").unwrap(), SslMode::VerifyFull);
        assert_eq!(SslMode::parse("VERIFY-CA").unwrap(), SslMode::VerifyCa);
        assert!(SslMode::parse("invalid").is_err());
    }

    // -- PGSSLROOTCERT env var resolution -----------------------------------

    #[test]
    #[serial]
    fn test_pgsslrootcert_env_var() {
        let _guard = EnvGuard::new(&[
            "PGHOST",
            "PGPORT",
            "PGDATABASE",
            "PGUSER",
            "PGSSLMODE",
            "PGSSLROOTCERT",
        ]);

        env::set_var("PGSSLROOTCERT", "/etc/ssl/certs/ca-certificates.crt");
        let opts = CliConnOpts::default();
        let params = resolve_params(&opts).unwrap();
        assert_eq!(
            params.ssl_root_cert,
            Some("/etc/ssl/certs/ca-certificates.crt".into())
        );
    }

    #[test]
    #[serial]
    fn test_pgsslrootcert_conninfo() {
        let _guard = EnvGuard::new(&[
            "PGHOST",
            "PGPORT",
            "PGDATABASE",
            "PGUSER",
            "PGSSLMODE",
            "PGSSLROOTCERT",
        ]);

        let opts = CliConnOpts {
            dbname_pos: Some("host=h sslrootcert=/tmp/ca.pem sslmode=verify-ca".into()),
            ..Default::default()
        };
        let params = resolve_params(&opts).unwrap();
        assert_eq!(params.ssl_root_cert, Some("/tmp/ca.pem".into()));
        assert_eq!(params.sslmode, SslMode::VerifyCa);
    }

    #[test]
    #[serial]
    fn test_pgsslrootcert_uri() {
        let _guard = EnvGuard::new(&[
            "PGHOST",
            "PGPORT",
            "PGDATABASE",
            "PGUSER",
            "PGSSLMODE",
            "PGSSLROOTCERT",
        ]);

        let opts = CliConnOpts {
            dbname_pos: Some(
                "postgresql://localhost/db?sslmode=verify-full&sslrootcert=/tmp/ca.pem".into(),
            ),
            ..Default::default()
        };
        let params = resolve_params(&opts).unwrap();
        assert_eq!(params.ssl_root_cert, Some("/tmp/ca.pem".into()));
        assert_eq!(params.sslmode, SslMode::VerifyFull);
    }

    #[test]
    #[serial]
    fn test_pgsslrootcert_not_set_by_default() {
        let _guard = EnvGuard::new(&["PGSSLROOTCERT"]);
        let opts = CliConnOpts::default();
        let params = resolve_params(&opts).unwrap();
        assert!(params.ssl_root_cert.is_none());
    }

    // -- application_name default -------------------------------------------

    #[test]
    #[serial]
    fn test_application_name_defaults_to_rpg() {
        let _guard = EnvGuard::new(&["PGAPPNAME"]);
        let opts = CliConnOpts::default();
        let params = resolve_params(&opts).unwrap();
        assert_eq!(params.application_name, "rpg");
    }

    // -- Flags override URI -------------------------------------------------

    #[test]
    #[serial]
    fn test_cli_flags_override_uri() {
        let _guard = EnvGuard::new(&["PGHOST", "PGPORT", "PGDATABASE", "PGUSER"]);

        let opts = CliConnOpts {
            host: Some("override-host".into()),
            dbname: Some("override-db".into()),
            dbname_pos: Some("postgresql://alice:pass@urihost:5433/uridb".into()),
            ..Default::default()
        };
        let params = resolve_params(&opts).unwrap();

        // CLI flags win over URI.
        assert_eq!(params.host, "override-host");
        assert_eq!(params.dbname, "override-db");
        // URI provides what flags don't.
        assert_eq!(params.user, "alice");
        assert_eq!(params.port, 5433);
        assert_eq!(params.password, Some("pass".into()));
    }

    // -- Helper: temporarily unset env vars for test isolation ---------------

    /// RAII guard that unsets environment variables on creation and restores
    /// them on drop.  This keeps tests deterministic regardless of the
    /// host environment.
    struct EnvGuard {
        saved: Vec<(String, Option<String>)>,
    }

    impl EnvGuard {
        fn new(vars: &[&str]) -> Self {
            let saved = vars
                .iter()
                .map(|&v| {
                    let prev = env::var(v).ok();
                    env::remove_var(v);
                    (v.to_owned(), prev)
                })
                .collect();
            Self { saved }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (var, val) in &self.saved {
                match val {
                    Some(v) => env::set_var(var, v),
                    None => env::remove_var(var),
                }
            }
        }
    }

    // -- connection_info format matches psql --------------------------------

    #[test]
    fn test_connection_info_tcp() {
        let params = ConnParams {
            host: "localhost".into(),
            port: 5432,
            user: "postgres".into(),
            dbname: "postgres".into(),
            ..ConnParams::default()
        };
        assert_eq!(
            connection_info(&params),
            "You are connected to database \"postgres\" as user \"postgres\" \
             on host \"localhost\" at port \"5432\".",
        );
    }

    #[test]
    fn test_connection_info_tcp_with_resolved_addr() {
        // When resolved_addr is set, psql-style (address "...") clause appears.
        let params = ConnParams {
            host: "localhost".into(),
            port: 5432,
            user: "nik".into(),
            dbname: "postgres".into(),
            resolved_addr: Some("127.0.0.1".into()),
            ..ConnParams::default()
        };
        assert_eq!(
            connection_info(&params),
            "You are connected to database \"postgres\" as user \"nik\" \
             on host \"localhost\" (address \"127.0.0.1\") at port \"5432\".",
        );
    }

    #[test]
    fn test_connection_info_socket() {
        let params = ConnParams {
            host: "/var/run/postgresql".into(),
            port: 5432,
            user: "alice".into(),
            dbname: "mydb".into(),
            ..ConnParams::default()
        };
        assert_eq!(
            connection_info(&params),
            "You are connected to database \"mydb\" as user \"alice\" \
             via socket in \"/var/run/postgresql\" at port \"5432\".",
        );
    }

    // -- reconnect_info format matches psql ----------------------------------

    #[test]
    fn test_reconnect_info_same_server_tcp() {
        // Same host/port → version banner always shown.
        let new = ConnParams {
            host: "localhost".into(),
            port: 5432,
            user: "alice".into(),
            dbname: "mydb".into(),
            ..ConnParams::default()
        };
        assert_eq!(
            reconnect_info("rpg 0.2.0 (abc1234, built 2026-01-01)", Some("17.2"), &new),
            "rpg 0.2.0 (abc1234, built 2026-01-01) (server PostgreSQL 17.2)\n\
             You are now connected to database \"mydb\" as user \"alice\" \
             on host \"localhost\" at port \"5432\".",
        );
    }

    #[test]
    fn test_reconnect_info_different_host_shows_version() {
        // Different host → version banner always prepended.
        let new = ConnParams {
            host: "other.example.com".into(),
            port: 5432,
            user: "alice".into(),
            dbname: "mydb".into(),
            ..ConnParams::default()
        };
        assert_eq!(
            reconnect_info("rpg 0.2.0 (abc1234, built 2026-01-01)", Some("16.3"), &new),
            "rpg 0.2.0 (abc1234, built 2026-01-01) (server PostgreSQL 16.3)\n\
             You are now connected to database \"mydb\" as user \"alice\" \
             on host \"other.example.com\" at port \"5432\".",
        );
    }

    #[test]
    fn test_reconnect_info_different_port_shows_version() {
        // Different port → version banner always prepended.
        let new = ConnParams {
            host: "localhost".into(),
            port: 5433,
            user: "postgres".into(),
            dbname: "mydb".into(),
            ..ConnParams::default()
        };
        assert_eq!(
            reconnect_info("rpg 0.2.0 (abc1234, built 2026-01-01)", Some("15.6"), &new),
            "rpg 0.2.0 (abc1234, built 2026-01-01) (server PostgreSQL 15.6)\n\
             You are now connected to database \"mydb\" as user \"postgres\" \
             on host \"localhost\" at port \"5433\".",
        );
    }

    #[test]
    fn test_reconnect_info_socket_same_server() {
        // Socket path → version banner always shown.
        let new = ConnParams {
            host: "/var/run/postgresql".into(),
            port: 5432,
            user: "alice".into(),
            dbname: "mydb".into(),
            ..ConnParams::default()
        };
        assert_eq!(
            reconnect_info("rpg 0.2.0 (abc1234, built 2026-01-01)", Some("17.2"), &new),
            "rpg 0.2.0 (abc1234, built 2026-01-01) (server PostgreSQL 17.2)\n\
             You are now connected to database \"mydb\" as user \"alice\" \
             via socket in \"/var/run/postgresql\" at port \"5432\".",
        );
    }

    #[test]
    fn test_reconnect_info_unknown_version() {
        // Server version unavailable → no banner, only connected line.
        let new = ConnParams {
            host: "other.host".into(),
            port: 5432,
            user: "postgres".into(),
            dbname: "postgres".into(),
            ..ConnParams::default()
        };
        assert_eq!(
            reconnect_info("rpg 0.2.0 (abc1234, built 2026-01-01)", None, &new),
            "You are now connected to database \"postgres\" as user \"postgres\" \
             on host \"other.host\" at port \"5432\".",
        );
    }

    // -- SSL / TLS status line --------------------------------------------

    #[test]
    fn test_tls_info_status_line() {
        let info = TlsInfo {
            protocol: "TLSv1.3".into(),
            cipher: "TLS_AES_256_GCM_SHA384".into(),
        };
        assert_eq!(
            info.status_line(),
            "SSL connection (protocol: TLSv1.3, cipher: TLS_AES_256_GCM_SHA384, \
             compression: off)",
        );
    }

    #[test]
    fn test_tls_info_status_line_tls12() {
        let info = TlsInfo {
            protocol: "TLSv1.2".into(),
            cipher: "TLS_ECDHE_RSA_WITH_AES_256_GCM_SHA384".into(),
        };
        assert_eq!(
            info.status_line(),
            "SSL connection (protocol: TLSv1.2, \
             cipher: TLS_ECDHE_RSA_WITH_AES_256_GCM_SHA384, compression: off)",
        );
    }

    #[test]
    fn test_protocol_version_str_tls13() {
        assert_eq!(
            protocol_version_str(rustls::ProtocolVersion::TLSv1_3),
            "TLSv1.3",
        );
    }

    #[test]
    fn test_protocol_version_str_tls12() {
        assert_eq!(
            protocol_version_str(rustls::ProtocolVersion::TLSv1_2),
            "TLSv1.2",
        );
    }

    #[test]
    fn test_cipher_suite_str_tls13() {
        // TLS 1.3 suites: rustls prefix "TLS13_" → IANA "TLS_"
        assert_eq!(
            cipher_suite_str(rustls::CipherSuite::TLS13_AES_256_GCM_SHA384),
            "TLS_AES_256_GCM_SHA384",
        );
        assert_eq!(
            cipher_suite_str(rustls::CipherSuite::TLS13_AES_128_GCM_SHA256),
            "TLS_AES_128_GCM_SHA256",
        );
        assert_eq!(
            cipher_suite_str(rustls::CipherSuite::TLS13_CHACHA20_POLY1305_SHA256),
            "TLS_CHACHA20_POLY1305_SHA256",
        );
    }

    #[test]
    fn test_cipher_suite_str_tls12() {
        // TLS 1.2 suites already start with "TLS_", no transformation needed.
        assert_eq!(
            cipher_suite_str(rustls::CipherSuite::TLS_ECDHE_RSA_WITH_AES_256_GCM_SHA384),
            "TLS_ECDHE_RSA_WITH_AES_256_GCM_SHA384",
        );
    }

    #[test]
    fn test_connection_info_tcp_with_tls() {
        let params = ConnParams {
            host: "db.example.com".into(),
            port: 5432,
            user: "alice".into(),
            dbname: "mydb".into(),
            tls_info: Some(TlsInfo {
                protocol: "TLSv1.3".into(),
                cipher: "TLS_AES_256_GCM_SHA384".into(),
            }),
            ..ConnParams::default()
        };
        assert_eq!(
            connection_info(&params),
            "You are connected to database \"mydb\" as user \"alice\" \
             on host \"db.example.com\" at port \"5432\".\n\
             SSL connection (protocol: TLSv1.3, cipher: TLS_AES_256_GCM_SHA384, \
             compression: off)",
        );
    }

    #[test]
    fn test_connection_info_socket_no_tls() {
        // Sockets never use TLS; tls_info must remain None.
        let params = ConnParams {
            host: "/var/run/postgresql".into(),
            port: 5432,
            user: "alice".into(),
            dbname: "mydb".into(),
            tls_info: None,
            ..ConnParams::default()
        };
        assert_eq!(
            connection_info(&params),
            "You are connected to database \"mydb\" as user \"alice\" \
             via socket in \"/var/run/postgresql\" at port \"5432\".",
        );
    }

    #[test]
    fn test_reconnect_info_same_server_with_tls() {
        // Same host/port + TLS → version banner + connected line + SSL.
        let new = ConnParams {
            host: "localhost".into(),
            port: 5432,
            user: "alice".into(),
            dbname: "mydb".into(),
            tls_info: Some(TlsInfo {
                protocol: "TLSv1.3".into(),
                cipher: "TLS_AES_256_GCM_SHA384".into(),
            }),
            ..ConnParams::default()
        };
        assert_eq!(
            reconnect_info("rpg 0.2.0 (abc1234, built 2026-01-01)", Some("17.2"), &new),
            "rpg 0.2.0 (abc1234, built 2026-01-01) (server PostgreSQL 17.2)\n\
             You are now connected to database \"mydb\" as user \"alice\" \
             on host \"localhost\" at port \"5432\".\n\
             SSL connection (protocol: TLSv1.3, cipher: TLS_AES_256_GCM_SHA384, \
             compression: off)",
        );
    }

    #[test]
    fn test_reconnect_info_different_host_with_tls() {
        // Different host + TLS → version banner then connected line then SSL.
        let new = ConnParams {
            host: "other.example.com".into(),
            port: 5432,
            user: "alice".into(),
            dbname: "mydb".into(),
            tls_info: Some(TlsInfo {
                protocol: "TLSv1.3".into(),
                cipher: "TLS_AES_256_GCM_SHA384".into(),
            }),
            ..ConnParams::default()
        };
        assert_eq!(
            reconnect_info("rpg 0.2.0 (abc1234, built 2026-01-01)", Some("16.3"), &new),
            "rpg 0.2.0 (abc1234, built 2026-01-01) (server PostgreSQL 16.3)\n\
             You are now connected to database \"mydb\" as user \"alice\" \
             on host \"other.example.com\" at port \"5432\".\n\
             SSL connection (protocol: TLSv1.3, cipher: TLS_AES_256_GCM_SHA384, \
             compression: off)",
        );
    }

    // -- PGSSLCERT / PGSSLKEY env var resolution ----------------------------

    #[test]
    #[serial]
    fn test_pgsslcert_pgsslkey_env_vars() {
        let _guard = EnvGuard::new(&[
            "PGHOST",
            "PGPORT",
            "PGDATABASE",
            "PGUSER",
            "PGSSLMODE",
            "PGSSLCERT",
            "PGSSLKEY",
        ]);

        env::set_var("PGSSLCERT", "/etc/ssl/client.crt");
        env::set_var("PGSSLKEY", "/etc/ssl/client.key");
        let opts = CliConnOpts::default();
        let params = resolve_params(&opts).unwrap();
        assert_eq!(params.ssl_cert, Some("/etc/ssl/client.crt".into()));
        assert_eq!(params.ssl_key, Some("/etc/ssl/client.key".into()));
    }

    #[test]
    #[serial]
    fn test_pgsslcert_pgsslkey_not_set_by_default() {
        let _guard = EnvGuard::new(&["PGSSLCERT", "PGSSLKEY"]);
        let opts = CliConnOpts::default();
        let params = resolve_params(&opts).unwrap();
        assert!(params.ssl_cert.is_none());
        assert!(params.ssl_key.is_none());
    }

    #[test]
    #[serial]
    fn test_sslcert_sslkey_uri_query_params() {
        let _guard = EnvGuard::new(&[
            "PGHOST",
            "PGPORT",
            "PGDATABASE",
            "PGUSER",
            "PGSSLMODE",
            "PGSSLCERT",
            "PGSSLKEY",
        ]);

        let opts = CliConnOpts {
            dbname_pos: Some(
                "postgresql://localhost/db?\
                 sslmode=verify-full\
                 &sslcert=/tmp/client.crt\
                 &sslkey=/tmp/client.key"
                    .into(),
            ),
            ..Default::default()
        };
        let params = resolve_params(&opts).unwrap();
        assert_eq!(params.ssl_cert, Some("/tmp/client.crt".into()));
        assert_eq!(params.ssl_key, Some("/tmp/client.key".into()));
        assert_eq!(params.sslmode, SslMode::VerifyFull);
    }

    #[test]
    #[serial]
    fn test_sslcert_sslkey_conninfo_keys() {
        let _guard = EnvGuard::new(&[
            "PGHOST",
            "PGPORT",
            "PGDATABASE",
            "PGUSER",
            "PGSSLMODE",
            "PGSSLCERT",
            "PGSSLKEY",
        ]);

        let opts = CliConnOpts {
            dbname_pos: Some(
                "host=h sslmode=verify-ca \
                 sslcert=/tmp/c.crt sslkey=/tmp/c.key"
                    .into(),
            ),
            ..Default::default()
        };
        let params = resolve_params(&opts).unwrap();
        assert_eq!(params.ssl_cert, Some("/tmp/c.crt".into()));
        assert_eq!(params.ssl_key, Some("/tmp/c.key".into()));
        assert_eq!(params.sslmode, SslMode::VerifyCa);
    }

    // -- PGOPTIONS / options resolution -------------------------------------

    #[test]
    #[serial]
    fn test_pgoptions_default_is_none() {
        let _guard = EnvGuard::new(&["PGOPTIONS"]);
        let opts = CliConnOpts::default();
        let params = resolve_params(&opts).unwrap();
        assert!(params.options.is_none());
    }

    #[test]
    #[serial]
    fn test_pgoptions_env_var() {
        let _guard = EnvGuard::new(&["PGHOST", "PGPORT", "PGDATABASE", "PGUSER", "PGOPTIONS"]);

        env::set_var("PGOPTIONS", "-c search_path=myschema");
        let opts = CliConnOpts::default();
        let params = resolve_params(&opts).unwrap();
        assert_eq!(params.options, Some("-c search_path=myschema".into()));
    }

    #[test]
    #[serial]
    fn test_options_conninfo_key() {
        let _guard = EnvGuard::new(&["PGHOST", "PGPORT", "PGDATABASE", "PGUSER", "PGOPTIONS"]);

        let opts = CliConnOpts {
            dbname_pos: Some("host=h options='-c search_path=myschema'".into()),
            ..Default::default()
        };
        let params = resolve_params(&opts).unwrap();
        assert_eq!(params.options, Some("-c search_path=myschema".into()),);
    }

    #[test]
    #[serial]
    fn test_options_uri_query_param() {
        let _guard = EnvGuard::new(&["PGHOST", "PGPORT", "PGDATABASE", "PGUSER", "PGOPTIONS"]);

        let opts = CliConnOpts {
            dbname_pos: Some(
                "postgresql://localhost/db?options=-c%20search_path%3Dmyschema".into(),
            ),
            ..Default::default()
        };
        let params = resolve_params(&opts).unwrap();
        assert_eq!(params.options, Some("-c search_path=myschema".into()),);
    }

    #[test]
    #[serial]
    fn test_options_uri_overrides_env() {
        let _guard = EnvGuard::new(&["PGHOST", "PGPORT", "PGDATABASE", "PGUSER", "PGOPTIONS"]);

        env::set_var("PGOPTIONS", "-c search_path=from_env");
        let opts = CliConnOpts {
            dbname_pos: Some(
                "postgresql://localhost/db?options=-c%20search_path%3Dfrom_uri".into(),
            ),
            ..Default::default()
        };
        let params = resolve_params(&opts).unwrap();
        assert_eq!(params.options, Some("-c search_path=from_uri".into()),);
    }

    // -- pg_service.conf parsing --------------------------------------------

    #[test]
    fn test_parse_service_file_basic() {
        let contents = "\
[myservice]
host=db.example.com
port=5433
dbname=mydb
user=alice
password=secret
";
        let all = parse_service_file(contents);
        let svc = all.get("myservice").unwrap();
        assert_eq!(svc.get("host").unwrap(), "db.example.com");
        assert_eq!(svc.get("port").unwrap(), "5433");
        assert_eq!(svc.get("dbname").unwrap(), "mydb");
        assert_eq!(svc.get("user").unwrap(), "alice");
        assert_eq!(svc.get("password").unwrap(), "secret");
    }

    #[test]
    fn test_parse_service_file_multiple_sections() {
        let contents = "\
[dev]
host=devhost
port=5432

[prod]
host=prodhost
port=5433
sslmode=verify-full
";
        let all = parse_service_file(contents);
        assert_eq!(all.get("dev").unwrap().get("host").unwrap(), "devhost");
        assert_eq!(all.get("prod").unwrap().get("host").unwrap(), "prodhost");
        assert_eq!(
            all.get("prod").unwrap().get("sslmode").unwrap(),
            "verify-full"
        );
    }

    // -- TargetSessionAttrs parsing -----------------------------------------

    #[test]
    fn test_target_session_attrs_parse_all_values() {
        assert_eq!(
            TargetSessionAttrs::parse("any").unwrap(),
            TargetSessionAttrs::Any
        );
        assert_eq!(
            TargetSessionAttrs::parse("read-write").unwrap(),
            TargetSessionAttrs::ReadWrite
        );
        assert_eq!(
            TargetSessionAttrs::parse("read_write").unwrap(),
            TargetSessionAttrs::ReadWrite
        );
        assert_eq!(
            TargetSessionAttrs::parse("read-only").unwrap(),
            TargetSessionAttrs::ReadOnly
        );
        assert_eq!(
            TargetSessionAttrs::parse("read_only").unwrap(),
            TargetSessionAttrs::ReadOnly
        );
        assert_eq!(
            TargetSessionAttrs::parse("primary").unwrap(),
            TargetSessionAttrs::Primary
        );
        assert_eq!(
            TargetSessionAttrs::parse("standby").unwrap(),
            TargetSessionAttrs::Standby
        );
        assert_eq!(
            TargetSessionAttrs::parse("prefer-standby").unwrap(),
            TargetSessionAttrs::PreferStandby
        );
        assert_eq!(
            TargetSessionAttrs::parse("prefer_standby").unwrap(),
            TargetSessionAttrs::PreferStandby
        );
        // Case-insensitive.
        assert_eq!(
            TargetSessionAttrs::parse("ANY").unwrap(),
            TargetSessionAttrs::Any
        );
        assert_eq!(
            TargetSessionAttrs::parse("READ-WRITE").unwrap(),
            TargetSessionAttrs::ReadWrite
        );
        // Unknown value → error.
        assert!(TargetSessionAttrs::parse("bogus").is_err());
    }

    #[test]
    fn test_target_session_attrs_display() {
        assert_eq!(TargetSessionAttrs::Any.to_string(), "any");
        assert_eq!(TargetSessionAttrs::ReadWrite.to_string(), "read-write");
        assert_eq!(TargetSessionAttrs::ReadOnly.to_string(), "read-only");
        assert_eq!(TargetSessionAttrs::Primary.to_string(), "primary");
        assert_eq!(TargetSessionAttrs::Standby.to_string(), "standby");
        assert_eq!(
            TargetSessionAttrs::PreferStandby.to_string(),
            "prefer-standby"
        );
    }

    #[test]
    fn test_parse_service_file_comments_and_blanks() {
        let contents = "\
# This is a comment

[myservice]
# Another comment
host=myhost

port=5432
";
        let all = parse_service_file(contents);
        let svc = all.get("myservice").unwrap();
        assert_eq!(svc.get("host").unwrap(), "myhost");
        assert_eq!(svc.get("port").unwrap(), "5432");
    }

    #[test]
    fn test_parse_service_file_unknown_keys_ignored() {
        let contents = "\
[myservice]
host=myhost
unknown_key=somevalue
another_unknown=foo
";
        let all = parse_service_file(contents);
        let svc = all.get("myservice").unwrap();
        assert_eq!(svc.get("host").unwrap(), "myhost");
        // Unknown keys must not appear.
        assert!(!svc.contains_key("unknown_key"));
        assert!(!svc.contains_key("another_unknown"));
    }

    #[test]
    fn test_parse_service_file_whitespace_around_equals() {
        let contents = "\
[svc]
host = trimmed-host
port = 5432
";
        let all = parse_service_file(contents);
        let svc = all.get("svc").unwrap();
        assert_eq!(svc.get("host").unwrap(), "trimmed-host");
        assert_eq!(svc.get("port").unwrap(), "5432");
    }

    #[test]
    fn test_parse_service_file_all_valid_keys() {
        let contents = "\
[full]
host=h
port=5432
dbname=db
user=u
password=pw
sslmode=require
sslrootcert=/ca.pem
sslcert=/c.pem
sslkey=/k.pem
application_name=myapp
connect_timeout=30
options=-c search_path=myschema
";
        let all = parse_service_file(contents);
        let svc = all.get("full").unwrap();
        assert_eq!(svc.len(), 12);
        assert_eq!(svc.get("application_name").unwrap(), "myapp");
        assert_eq!(svc.get("connect_timeout").unwrap(), "30");
        assert_eq!(svc.get("options").unwrap(), "-c search_path=myschema");
    }

    #[test]
    fn test_parse_service_file_empty_file() {
        let all = parse_service_file("");
        assert!(all.is_empty());
    }

    #[test]
    fn test_parse_service_file_no_key_before_section() {
        // Lines before the first section header are ignored.
        let contents = "\
orphan_key=value
[svc]
host=myhost
";
        let all = parse_service_file(contents);
        assert!(!all.contains_key(""));
        assert_eq!(all.get("svc").unwrap().get("host").unwrap(), "myhost");
    }

    // -- PGSERVICE env var → service resolution -----------------------------

    #[test]
    #[serial]
    fn test_pgservice_env_var_selects_service() {
        let _guard = EnvGuard::new(&[
            "PGHOST",
            "PGPORT",
            "PGDATABASE",
            "PGUSER",
            "PGPASSWORD",
            "PGSSLMODE",
            "PGSERVICE",
            "PGSERVICEFILE",
        ]);

        // Write a temporary service file.
        let tmp = std::env::temp_dir().join("rpg_test_service.conf");
        std::fs::write(
            &tmp,
            "[testservice]\nhost=svchost\nport=5555\ndbname=svcdb\nuser=svcuser\n",
        )
        .unwrap();

        env::set_var("PGSERVICEFILE", tmp.to_str().unwrap());
        env::set_var("PGSERVICE", "testservice");

        let opts = CliConnOpts::default();
        let params = resolve_params(&opts).unwrap();

        assert_eq!(params.host, "svchost");
        assert_eq!(params.port, 5555);
        assert_eq!(params.dbname, "svcdb");
        assert_eq!(params.user, "svcuser");

        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    #[serial]
    fn test_target_session_attrs_default_is_any() {
        let _guard = EnvGuard::new(&["PGTARGETSESSIONATTRS"]);
        let opts = CliConnOpts::default();
        let params = resolve_params(&opts).unwrap();
        assert_eq!(params.target_session_attrs, TargetSessionAttrs::Any);
    }

    #[test]
    #[serial]
    fn test_target_session_attrs_from_env() {
        let _guard = EnvGuard::new(&["PGTARGETSESSIONATTRS"]);
        env::set_var("PGTARGETSESSIONATTRS", "read-write");
        let opts = CliConnOpts::default();
        let params = resolve_params(&opts).unwrap();
        assert_eq!(params.target_session_attrs, TargetSessionAttrs::ReadWrite);
    }

    #[test]
    #[serial]
    fn test_conninfo_service_key_selects_service() {
        let _guard = EnvGuard::new(&[
            "PGHOST",
            "PGPORT",
            "PGDATABASE",
            "PGUSER",
            "PGPASSWORD",
            "PGSSLMODE",
            "PGSERVICE",
            "PGSERVICEFILE",
        ]);

        let tmp = std::env::temp_dir().join("rpg_test_service2.conf");
        std::fs::write(
            &tmp,
            "[ci]\nhost=cihost\nport=6543\ndbname=cidb\nuser=ciuser\n",
        )
        .unwrap();

        env::set_var("PGSERVICEFILE", tmp.to_str().unwrap());

        let opts = CliConnOpts {
            dbname_pos: Some("service=ci".into()),
            ..Default::default()
        };
        let params = resolve_params(&opts).unwrap();

        assert_eq!(params.host, "cihost");
        assert_eq!(params.port, 6543);
        assert_eq!(params.dbname, "cidb");
        assert_eq!(params.user, "ciuser");

        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    #[serial]
    fn test_target_session_attrs_from_uri() {
        let _guard = EnvGuard::new(&[
            "PGHOST",
            "PGPORT",
            "PGDATABASE",
            "PGUSER",
            "PGTARGETSESSIONATTRS",
        ]);
        let opts = CliConnOpts {
            dbname_pos: Some("postgresql://h1,h2/db?target_session_attrs=standby".into()),
            ..Default::default()
        };
        let params = resolve_params(&opts).unwrap();
        assert_eq!(params.target_session_attrs, TargetSessionAttrs::Standby);
    }

    #[test]
    #[serial]
    fn test_cli_flag_overrides_service() {
        let _guard = EnvGuard::new(&[
            "PGHOST",
            "PGPORT",
            "PGDATABASE",
            "PGUSER",
            "PGPASSWORD",
            "PGSSLMODE",
            "PGSERVICE",
            "PGSERVICEFILE",
        ]);

        let tmp = std::env::temp_dir().join("rpg_test_service3.conf");
        std::fs::write(
            &tmp,
            "[svc]\nhost=svchost\nport=5432\ndbname=svcdb\nuser=svcuser\n",
        )
        .unwrap();

        env::set_var("PGSERVICEFILE", tmp.to_str().unwrap());
        env::set_var("PGSERVICE", "svc");

        let opts = CliConnOpts {
            host: Some("override-host".into()),
            dbname: Some("override-db".into()),
            ..Default::default()
        };
        let params = resolve_params(&opts).unwrap();

        // CLI flags win.
        assert_eq!(params.host, "override-host");
        assert_eq!(params.dbname, "override-db");
        // Service provides the rest.
        assert_eq!(params.user, "svcuser");
        assert_eq!(params.port, 5432);

        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    #[serial]
    fn test_target_session_attrs_from_conninfo() {
        let _guard = EnvGuard::new(&[
            "PGHOST",
            "PGPORT",
            "PGDATABASE",
            "PGUSER",
            "PGTARGETSESSIONATTRS",
        ]);
        let opts = CliConnOpts {
            dbname_pos: Some("host=h1,h2 target_session_attrs=primary".into()),
            ..Default::default()
        };
        let params = resolve_params(&opts).unwrap();
        assert_eq!(params.target_session_attrs, TargetSessionAttrs::Primary);
    }

    #[test]
    #[serial]
    fn test_service_overrides_env_var() {
        // Service file params act at the conninfo level and therefore
        // override env vars — matching libpq / psql behaviour.
        let _guard = EnvGuard::new(&[
            "PGHOST",
            "PGPORT",
            "PGDATABASE",
            "PGUSER",
            "PGPASSWORD",
            "PGSSLMODE",
            "PGSERVICE",
            "PGSERVICEFILE",
        ]);

        let tmp = std::env::temp_dir().join("rpg_test_service4.conf");
        std::fs::write(&tmp, "[svc]\nhost=svchost\nport=5432\n").unwrap();

        env::set_var("PGSERVICEFILE", tmp.to_str().unwrap());
        env::set_var("PGSERVICE", "svc");
        // PGHOST is set but service file should take precedence.
        env::set_var("PGHOST", "env-host");

        let opts = CliConnOpts::default();
        let params = resolve_params(&opts).unwrap();

        // Service file wins over PGHOST env var.
        assert_eq!(params.host, "svchost");
        assert_eq!(params.port, 5432);

        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    #[serial]
    fn test_missing_service_name_returns_error() {
        let _guard = EnvGuard::new(&[
            "PGHOST",
            "PGPORT",
            "PGDATABASE",
            "PGUSER",
            "PGSERVICE",
            "PGSERVICEFILE",
        ]);

        let tmp = std::env::temp_dir().join("rpg_test_service5.conf");
        std::fs::write(&tmp, "[other]\nhost=otherhost\n").unwrap();

        env::set_var("PGSERVICEFILE", tmp.to_str().unwrap());
        env::set_var("PGSERVICE", "nonexistent");

        let opts = CliConnOpts::default();
        let result = resolve_params(&opts);
        assert!(result.is_err(), "expected error for missing service name");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("nonexistent"),
            "error should mention service name"
        );

        let _ = std::fs::remove_file(&tmp);
    }

    // -- Multi-host URI parsing ---------------------------------------------

    #[test]
    fn test_parse_uri_multihost_no_ports() {
        let up = parse_uri("postgresql://h1,h2,h3/db").unwrap();
        assert_eq!(
            up.hosts,
            vec![
                ("h1".to_owned(), 5432),
                ("h2".to_owned(), 5432),
                ("h3".to_owned(), 5432),
            ]
        );
        assert_eq!(up.host, Some("h1".to_owned()));
        assert_eq!(up.port, Some(5432));
        assert_eq!(up.dbname, Some("db".to_owned()));
    }

    #[test]
    fn test_parse_uri_multihost_with_ports() {
        let up = parse_uri("postgresql://h1:5432,h2:5433,h3:5434/db").unwrap();
        assert_eq!(
            up.hosts,
            vec![
                ("h1".to_owned(), 5432),
                ("h2".to_owned(), 5433),
                ("h3".to_owned(), 5434),
            ]
        );
    }

    #[test]
    fn test_parse_uri_multihost_port_reuse() {
        // Last explicit port is reused for hosts without one.
        let up = parse_uri("postgresql://h1:5432,h2,h3/db").unwrap();
        assert_eq!(
            up.hosts,
            vec![
                ("h1".to_owned(), 5432),
                ("h2".to_owned(), 5432),
                ("h3".to_owned(), 5432),
            ]
        );
    }

    #[test]
    fn test_parse_uri_single_host_populates_hosts() {
        let up = parse_uri("postgresql://myhost:5433/db").unwrap();
        assert_eq!(up.hosts, vec![("myhost".to_owned(), 5433)]);
    }

    #[test]
    #[serial]
    fn test_resolve_params_multihost_uri() {
        let _guard = EnvGuard::new(&[
            "PGHOST",
            "PGPORT",
            "PGDATABASE",
            "PGUSER",
            "PGTARGETSESSIONATTRS",
        ]);
        let opts = CliConnOpts {
            dbname_pos: Some("postgresql://h1:5432,h2:5433,h3/db".into()),
            ..Default::default()
        };
        let params = resolve_params(&opts).unwrap();
        assert_eq!(
            params.hosts,
            vec![
                ("h1".to_owned(), 5432),
                ("h2".to_owned(), 5433),
                // tokio-postgres URI parser assigns 5432 to hosts without an explicit port,
                // matching real libpq behavior (not the last-port-reuse behavior rpg previously had).
                ("h3".to_owned(), 5432),
            ]
        );
        // host/port reflect the first entry.
        assert_eq!(params.host, "h1");
        assert_eq!(params.port, 5432);
    }

    // -- Multi-host conninfo parsing ----------------------------------------

    #[test]
    #[serial]
    fn test_resolve_params_multihost_conninfo() {
        let _guard = EnvGuard::new(&[
            "PGHOST",
            "PGPORT",
            "PGDATABASE",
            "PGUSER",
            "PGTARGETSESSIONATTRS",
        ]);
        let opts = CliConnOpts {
            dbname_pos: Some("host=h1,h2,h3 port=5432,5433 dbname=mydb".into()),
            ..Default::default()
        };
        let params = resolve_params(&opts).unwrap();
        assert_eq!(
            params.hosts,
            vec![
                ("h1".to_owned(), 5432),
                ("h2".to_owned(), 5433),
                ("h3".to_owned(), 5433), // port reused
            ]
        );
    }

    #[test]
    #[serial]
    fn test_resolve_params_multihost_conninfo_single_port() {
        let _guard = EnvGuard::new(&[
            "PGHOST",
            "PGPORT",
            "PGDATABASE",
            "PGUSER",
            "PGTARGETSESSIONATTRS",
        ]);
        let opts = CliConnOpts {
            dbname_pos: Some("host=h1,h2,h3 port=6543 dbname=mydb".into()),
            ..Default::default()
        };
        let params = resolve_params(&opts).unwrap();
        assert_eq!(
            params.hosts,
            vec![
                ("h1".to_owned(), 6543),
                ("h2".to_owned(), 6543),
                ("h3".to_owned(), 6543),
            ]
        );
    }

    #[test]
    #[serial]
    fn test_resolve_params_single_host_populates_hosts() {
        let _guard = EnvGuard::new(&[
            "PGHOST",
            "PGPORT",
            "PGDATABASE",
            "PGUSER",
            "PGTARGETSESSIONATTRS",
        ]);
        let opts = CliConnOpts {
            dbname_pos: Some("postgresql://myhost:9999/mydb".into()),
            ..Default::default()
        };
        let params = resolve_params(&opts).unwrap();
        assert_eq!(params.hosts, vec![("myhost".to_owned(), 9999)]);
    }

    // -- CLI multi-host parsing (#743) --------------------------------------

    /// CLI `-h h1,h2 -p 5432` — single port applied to all hosts.
    #[test]
    #[serial]
    fn test_resolve_params_cli_multihost_single_port() {
        let _guard = EnvGuard::new(&["PGHOST", "PGPORT", "PGDATABASE", "PGUSER"]);
        let opts = CliConnOpts {
            host: Some("h1,h2,h3".into()),
            port: Some(6543),
            port_str: Some("6543".into()),
            ..Default::default()
        };
        let params = resolve_params(&opts).unwrap();
        assert_eq!(
            params.hosts,
            vec![
                ("h1".to_owned(), 6543),
                ("h2".to_owned(), 6543),
                ("h3".to_owned(), 6543),
            ]
        );
        // host/port reflect the first entry.
        assert_eq!(params.host, "h1");
        assert_eq!(params.port, 6543);
    }

    /// CLI `-h h1,h2 -p 5432,5433` — per-host ports.
    #[test]
    #[serial]
    fn test_resolve_params_cli_multihost_per_host_ports() {
        let _guard = EnvGuard::new(&["PGHOST", "PGPORT", "PGDATABASE", "PGUSER"]);
        let opts = CliConnOpts {
            host: Some("h1,h2,h3".into()),
            port: Some(5432),
            port_str: Some("5432,5433,5434".into()),
            ..Default::default()
        };
        let params = resolve_params(&opts).unwrap();
        assert_eq!(
            params.hosts,
            vec![
                ("h1".to_owned(), 5432),
                ("h2".to_owned(), 5433),
                ("h3".to_owned(), 5434),
            ]
        );
    }

    /// CLI `-h h1,h2 -p 5432,5433` — port reuse for remaining hosts
    /// when fewer ports than hosts are provided.
    #[test]
    #[serial]
    fn test_resolve_params_cli_multihost_port_reuse() {
        let _guard = EnvGuard::new(&["PGHOST", "PGPORT", "PGDATABASE", "PGUSER"]);
        let opts = CliConnOpts {
            host: Some("h1,h2,h3".into()),
            port: Some(5432),
            port_str: Some("5432,5433".into()),
            ..Default::default()
        };
        let params = resolve_params(&opts).unwrap();
        assert_eq!(
            params.hosts,
            vec![
                ("h1".to_owned(), 5432),
                ("h2".to_owned(), 5433),
                ("h3".to_owned(), 5433), // last port reused
            ]
        );
    }

    /// Positional HOST with commas also works (HOST arg).
    #[test]
    #[serial]
    fn test_resolve_params_cli_multihost_positional() {
        let _guard = EnvGuard::new(&["PGHOST", "PGPORT", "PGDATABASE", "PGUSER"]);
        let opts = CliConnOpts {
            dbname_pos: Some("testdb".into()),
            host_pos: Some("h1,h2".into()),
            port_pos: Some("7777,7778".into()),
            ..Default::default()
        };
        let params = resolve_params(&opts).unwrap();
        assert_eq!(
            params.hosts,
            vec![("h1".to_owned(), 7777), ("h2".to_owned(), 7778),]
        );
    }

    /// Invalid port in `-p` must not silently connect to the wrong port.
    ///
    /// When `-p 5432,notaport` is supplied, the invalid token is detected and
    /// the multi-host path is abandoned.  `resolve_params` must not panic and
    /// must not silently produce a host list with port 0.
    #[test]
    #[serial]
    fn test_resolve_params_cli_multihost_invalid_port_does_not_silently_use_wrong_port() {
        let _guard = EnvGuard::new(&["PGHOST", "PGPORT", "PGDATABASE", "PGUSER"]);
        let opts = CliConnOpts {
            host: Some("h1,h2".into()),
            port: Some(5432),
            port_str: Some("5432,notaport".into()),
            ..Default::default()
        };
        // Should not panic; the invalid port causes the multi-host path to be
        // abandoned.  The resulting host list must reflect the single-host
        // fallback (params.host / params.port), not stale defaults.
        let params = resolve_params(&opts).unwrap();
        assert_eq!(
            params.hosts,
            vec![(params.host.clone(), params.port)],
            "hosts must contain the single-host fallback on invalid port"
        );
        assert_eq!(
            params.port, 5432,
            "port must be the resolved single-host port"
        );
        assert!(!params.host.is_empty(), "host must not be empty");
    }

    /// Single CLI host still works as before (regression guard).
    #[test]
    #[serial]
    fn test_resolve_params_cli_single_host_regression() {
        let _guard = EnvGuard::new(&["PGHOST", "PGPORT", "PGDATABASE", "PGUSER"]);
        let opts = CliConnOpts {
            host: Some("myhost".into()),
            port: Some(9999),
            port_str: Some("9999".into()),
            ..Default::default()
        };
        let params = resolve_params(&opts).unwrap();
        assert_eq!(params.hosts, vec![("myhost".to_owned(), 9999)]);
        assert_eq!(params.host, "myhost");
        assert_eq!(params.port, 9999);
    }

    // -- SSL error classification -------------------------------------------

    fn dummy_params() -> ConnParams {
        ConnParams {
            host: "testhost".to_owned(),
            port: 5432,
            ..Default::default()
        }
    }

    #[test]
    fn test_classify_ssl_not_supported() {
        // tokio-postgres emits this when the server returns 'N' to SSLRequest.
        let params = dummy_params();
        let err = classify_connect_error("server does not support TLS".to_owned(), &params);
        assert!(
            matches!(err, ConnectionError::SslNotSupported),
            "expected SslNotSupported, got: {err:?}"
        );
        assert_eq!(err.to_string(), "SSL error: server does not support SSL");
    }

    #[test]
    fn test_classify_ssl_cert_verification_failed_invalid_peer() {
        // rustls emits "invalid peer certificate: ..." for chain/CA errors.
        let params = dummy_params();
        let detail = "invalid peer certificate: UnknownIssuer".to_owned();
        let err = classify_connect_error(detail.clone(), &params);
        assert!(
            matches!(err, ConnectionError::SslCertVerificationFailed(_)),
            "expected SslCertVerificationFailed, got: {err:?}"
        );
        assert!(err.to_string().contains("certificate verification failed"));
        assert!(err.to_string().contains("UnknownIssuer"));
    }

    #[test]
    fn test_classify_ssl_cert_verification_failed_hostname() {
        // rustls emits "invalid peer certificate: ..." for hostname mismatch.
        let params = dummy_params();
        let detail = "invalid peer certificate: NotValidForName".to_owned();
        let err = classify_connect_error(detail, &params);
        assert!(
            matches!(err, ConnectionError::SslCertVerificationFailed(_)),
            "expected SslCertVerificationFailed, got: {err:?}"
        );
    }

    #[test]
    fn test_classify_ssl_required_by_server() {
        // Server-side "require SSL" message (when client tries plain
        // connection but server has ssl=on + pg_hba requires it).
        let params = dummy_params();
        let err = classify_connect_error("SSL connection is required".to_owned(), &params);
        assert!(
            matches!(err, ConnectionError::SslRequired),
            "expected SslRequired, got: {err:?}"
        );
    }

    #[test]
    fn test_classify_generic_tls_error() {
        // Any other TLS error falls through to the generic TlsError variant.
        let params = dummy_params();
        let err = classify_connect_error(
            "received fatal TLS alert: HandshakeFailure".to_owned(),
            &params,
        );
        assert!(
            matches!(err, ConnectionError::TlsError(_)),
            "expected TlsError, got: {err:?}"
        );
        assert!(err.to_string().starts_with("SSL error:"));
    }

    #[test]
    fn test_classify_auth_error() {
        let params = dummy_params();
        let err = classify_connect_error(
            "password authentication failed for user \"postgres\"".to_owned(),
            &params,
        );
        assert!(
            matches!(err, ConnectionError::AuthenticationFailed { .. }),
            "expected AuthenticationFailed, got: {err:?}"
        );
    }

    #[test]
    fn test_classify_connection_failed() {
        let params = dummy_params();
        let err = classify_connect_error("Connection refused (os error 111)".to_owned(), &params);
        assert!(
            matches!(err, ConnectionError::ConnectionFailed { .. }),
            "expected ConnectionFailed, got: {err:?}"
        );
    }
}
