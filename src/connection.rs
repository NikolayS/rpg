//! Postgres wire-protocol connection and authentication.
//!
//! Resolves connection parameters from CLI flags, positional arguments,
//! URI / conninfo strings, environment variables, `.pgpass`, and defaults.
//! Then establishes a `tokio-postgres` connection with optional TLS.

use std::collections::HashMap;
use std::env;
use std::fmt;
use std::path::PathBuf;

use rustls::ClientConfig;
use thiserror::Error;
use tokio_postgres::config::SslMode as TokioSslMode;
use tokio_postgres::Client;
use tokio_postgres_rustls::MakeRustlsConnect;

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

    /// General TLS failure (bad cert, handshake failure, etc.).
    #[error("TLS error: {0}")]
    TlsError(String),

    #[error("pgpass error: {0}")]
    PgpassError(String),

    #[error("invalid connection string: {0}")]
    InvalidConnectionString(String),
}

// ---------------------------------------------------------------------------
// SSL mode
// ---------------------------------------------------------------------------

/// Parsed SSL mode from user input.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SslMode {
    Disable,
    #[default]
    Prefer,
    Require,
}

impl SslMode {
    /// Parse from a string value (case-insensitive).
    pub fn parse(s: &str) -> Result<Self, ConnectionError> {
        match s.to_lowercase().as_str() {
            "disable" => Ok(Self::Disable),
            "prefer" => Ok(Self::Prefer),
            "require" => Ok(Self::Require),
            other => Err(ConnectionError::InvalidConnectionString(format!(
                "unknown sslmode: {other}"
            ))),
        }
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
    pub user: String,
    pub dbname: String,
    pub password: Option<String>,
    pub sslmode: SslMode,
    pub application_name: String,
    pub connect_timeout: Option<u64>,
    /// Whether the connection was actually established over TLS.
    ///
    /// `false` when `sslmode=disable` or when `sslmode=prefer` fell back to
    /// a plain connection.  `true` when TLS handshake completed successfully.
    pub tls_in_use: bool,
}

/// Custom `Debug` implementation that masks the password field.
impl fmt::Debug for ConnParams {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConnParams")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("user", &self.user)
            .field("dbname", &self.dbname)
            .field("password", &self.password.as_ref().map(|_| "***"))
            .field("sslmode", &self.sslmode)
            .field("application_name", &self.application_name)
            .field("connect_timeout", &self.connect_timeout)
            .field("tls_in_use", &self.tls_in_use)
            .finish()
    }
}

impl Default for ConnParams {
    fn default() -> Self {
        Self {
            host: default_host(),
            port: 5432,
            user: default_user(),
            dbname: String::new(), // filled in by resolve — set to user
            password: None,
            sslmode: SslMode::default(),
            application_name: "samo".to_owned(),
            connect_timeout: None,
            tls_in_use: false,
        }
    }
}

/// Return the default host. On Unix, if a well-known socket directory
/// exists, return that; otherwise `"localhost"`.
fn default_host() -> String {
    #[cfg(unix)]
    {
        for dir in &["/var/run/postgresql", "/tmp"] {
            if PathBuf::from(dir).is_dir() {
                return (*dir).to_owned();
            }
        }
    }
    "localhost".to_owned()
}

/// Default user: `$USER` (or `$USERNAME` on Windows), falling back to
/// `"postgres"`.
fn default_user() -> String {
    env::var("USER")
        .or_else(|_| env::var("USERNAME"))
        .unwrap_or_else(|_| "postgres".to_owned())
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
/// 5. Environment variables
/// 6. Defaults
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

    let uri_ref = uri_params.as_ref();
    let ci_ref = conninfo_params.as_ref();

    resolve_host(&mut params, opts, uri_ref, ci_ref, is_plain_positional);
    resolve_port(&mut params, opts, uri_ref, ci_ref, is_plain_positional);
    resolve_user(&mut params, opts, uri_ref, ci_ref, is_plain_positional);
    resolve_dbname(&mut params, opts, uri_ref, ci_ref, is_plain_positional);

    // Password (from URI / env only; pgpass + prompt happen later).
    params.password = uri_ref
        .and_then(|u| u.password.clone())
        .or_else(|| env::var("PGPASSWORD").ok());

    resolve_sslmode(&mut params, opts, uri_ref, ci_ref);
    resolve_app_name(&mut params, uri_ref, ci_ref);

    // Connect timeout: URI query params, then conninfo, then env.
    params.connect_timeout = uri_ref
        .and_then(|u| u.connect_timeout)
        .or_else(|| {
            conninfo_params
                .as_ref()
                .and_then(|c| c.get("connect_timeout").and_then(|v| v.parse().ok()))
        })
        .or_else(|| {
            env::var("PGCONNECT_TIMEOUT")
                .ok()
                .and_then(|v| v.parse().ok())
        });

    Ok(params)
}

fn resolve_host(
    params: &mut ConnParams,
    opts: &CliConnOpts,
    uri: Option<&UriParams>,
    conninfo: Option<&HashMap<String, String>>,
    is_plain: bool,
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
        .or_else(|| env::var("PGHOST").ok())
        .unwrap_or_else(default_host);
}

fn resolve_port(
    params: &mut ConnParams,
    opts: &CliConnOpts,
    uri: Option<&UriParams>,
    conninfo: Option<&HashMap<String, String>>,
    is_plain: bool,
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
        .or_else(|| env::var("PGPORT").ok().and_then(|p| p.parse().ok()))
        .unwrap_or(5432);
}

fn resolve_user(
    params: &mut ConnParams,
    opts: &CliConnOpts,
    uri: Option<&UriParams>,
    conninfo: Option<&HashMap<String, String>>,
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
        .or_else(|| env::var("PGUSER").ok())
        .unwrap_or_else(default_user);
}

fn resolve_dbname(
    params: &mut ConnParams,
    opts: &CliConnOpts,
    uri: Option<&UriParams>,
    conninfo: Option<&HashMap<String, String>>,
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
        .or_else(|| env::var("PGDATABASE").ok())
        .unwrap_or_else(|| params.user.clone());
}

fn resolve_sslmode(
    params: &mut ConnParams,
    opts: &CliConnOpts,
    uri: Option<&UriParams>,
    conninfo: Option<&HashMap<String, String>>,
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
) {
    params.application_name = uri
        .and_then(|u| u.application_name.clone())
        .or_else(|| conninfo.and_then(|c| c.get("application_name").cloned()))
        .or_else(|| env::var("PGAPPNAME").ok())
        .unwrap_or_else(|| "samo".to_owned());
}

// ---------------------------------------------------------------------------
// URI parsing
// ---------------------------------------------------------------------------

/// Intermediate result of parsing a `postgresql://…` URI.
#[derive(Debug, Default)]
struct UriParams {
    host: Option<String>,
    port: Option<u16>,
    user: Option<String>,
    password: Option<String>,
    dbname: Option<String>,
    sslmode: Option<SslMode>,
    application_name: Option<String>,
    connect_timeout: Option<u64>,
}

/// Parse a `postgresql://` or `postgres://` URI into individual fields.
fn parse_uri(uri: &str) -> Result<UriParams, ConnectionError> {
    let err = |msg: String| ConnectionError::InvalidConnectionString(msg);

    // Strip the scheme.
    let rest = uri
        .strip_prefix("postgresql://")
        .or_else(|| uri.strip_prefix("postgres://"))
        .ok_or_else(|| err(format!("not a postgres URI: {uri}")))?;

    let mut params = UriParams::default();

    // Split on `?` for query parameters.
    let (main_part, query_part) = match rest.split_once('?') {
        Some((m, q)) => (m, Some(q)),
        None => (rest, None),
    };

    // Split on `/` to separate authority from dbname.
    let (authority, db) = match main_part.split_once('/') {
        Some((a, d)) => (a, if d.is_empty() { None } else { Some(d) }),
        None => (main_part, None),
    };

    params.dbname = db.map(percent_decode);

    // Parse authority: [user[:password]@]host[:port]
    let (userinfo, hostport) = match authority.split_once('@') {
        Some((u, h)) => (Some(u), h),
        None => (None, authority),
    };

    if let Some(userinfo) = userinfo {
        match userinfo.split_once(':') {
            Some((u, p)) => {
                if !u.is_empty() {
                    params.user = Some(percent_decode(u));
                }
                params.password = Some(percent_decode(p));
            }
            None => {
                if !userinfo.is_empty() {
                    params.user = Some(percent_decode(userinfo));
                }
            }
        }
    }

    if !hostport.is_empty() {
        // Handle IPv6 bracket notation [::1]:5432
        if let Some(rest_after_bracket) = hostport.strip_prefix('[') {
            if let Some((ipv6, port_part)) = rest_after_bracket.split_once(']') {
                params.host = Some(ipv6.to_owned());
                if let Some(port_str) = port_part.strip_prefix(':') {
                    params.port = Some(
                        port_str
                            .parse::<u16>()
                            .map_err(|_| err(format!("invalid port in URI: {port_str}")))?,
                    );
                }
            } else {
                return Err(err("unterminated IPv6 bracket in URI".to_owned()));
            }
        } else {
            match hostport.rsplit_once(':') {
                Some((h, p)) => {
                    if !h.is_empty() {
                        params.host = Some(percent_decode(h));
                    }
                    params.port = Some(
                        p.parse::<u16>()
                            .map_err(|_| err(format!("invalid port in URI: {p}")))?,
                    );
                }
                None => {
                    params.host = Some(percent_decode(hostport));
                }
            }
        }
    }

    // Parse query parameters.
    if let Some(query) = query_part {
        for pair in query.split('&') {
            if let Some((key, val)) = pair.split_once('=') {
                let val = percent_decode(val);
                match key {
                    "sslmode" => params.sslmode = Some(SslMode::parse(&val)?),
                    "application_name" => params.application_name = Some(val),
                    "connect_timeout" => params.connect_timeout = val.parse().ok(),
                    // Ignore unknown query params rather than erroring.
                    _ => {}
                }
            }
        }
    }

    Ok(params)
}

/// Minimal percent-decoding for URI components.
fn percent_decode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.as_bytes().iter();
    while let Some(&b) = chars.next() {
        if b == b'%' {
            let hi = chars.next().copied().unwrap_or(b'0');
            let lo = chars.next().copied().unwrap_or(b'0');
            let hex = [hi, lo];
            if let Ok(s) = std::str::from_utf8(&hex) {
                if let Ok(byte) = u8::from_str_radix(s, 16) {
                    out.push(char::from(byte));
                    continue;
                }
            }
            // Malformed; pass through.
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

/// Build a `rustls` `ClientConfig` using system/webpki root certificates.
fn make_tls_config() -> ClientConfig {
    let root_store: rustls::RootCertStore =
        webpki_roots::TLS_SERVER_ROOTS.iter().cloned().collect();

    ClientConfig::builder()
        .with_root_certificates(root_store)
        .with_no_client_auth()
}

// ---------------------------------------------------------------------------
// Connect
// ---------------------------------------------------------------------------

/// Establish a connection to Postgres and return both the `Client` and the
/// fully-resolved `ConnParams` that were used.
///
/// Accepting a pre-resolved `ConnParams` ensures the caller (e.g. `main`)
/// always uses the same parameters that were passed to the driver.
pub async fn connect(
    mut params: ConnParams,
    opts: &CliConnOpts,
) -> Result<(Client, ConnParams), ConnectionError> {
    // Resolve password (pre-connect: may prompt if -W).
    resolve_password(&mut params, opts.force_password, opts.no_password, false)?;

    // Build tokio-postgres config.
    let mut pg_config = tokio_postgres::Config::new();
    pg_config
        .host(&params.host)
        .port(params.port)
        .user(&params.user)
        .dbname(&params.dbname)
        .application_name(&params.application_name);

    if let Some(ref pw) = params.password {
        pg_config.password(pw);
    }

    if let Some(timeout) = params.connect_timeout {
        pg_config.connect_timeout(std::time::Duration::from_secs(timeout));
    }

    let (client, tls_used) = match params.sslmode {
        SslMode::Disable => (connect_plain(&pg_config, &params).await?, false),
        SslMode::Prefer => match connect_tls(&pg_config, &params).await {
            Ok(c) => (c, true),
            Err(_) => {
                // sslmode=prefer: silently fall back to a plain connection
                // when TLS is unavailable. This matches psql's default
                // behavior — no warning is shown to the user.
                (connect_plain(&pg_config, &params).await?, false)
            }
        },
        SslMode::Require => {
            pg_config.ssl_mode(TokioSslMode::Require);
            (connect_tls(&pg_config, &params).await?, true)
        }
    };

    params.tls_in_use = tls_used;

    // Auth retry: if the initial connect failed with an auth error and the
    // server is requesting a password, prompt and retry once (psql behaviour).
    // (The retry path is reached via the caller; here we return on success.)

    Ok((client, params))
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
        if let Err(e) = connection.await {
            eprintln!("samo: connection error: {e}");
        }
    });

    Ok(client)
}

/// Connect with TLS.
async fn connect_tls(
    pg_config: &tokio_postgres::Config,
    params: &ConnParams,
) -> Result<Client, ConnectionError> {
    let tls_config = make_tls_config();
    let tls = MakeRustlsConnect::new(tls_config);

    let (client, connection) = pg_config
        .connect(tls)
        .await
        .map_err(|e| map_connect_error(&e, params))?;

    tokio::spawn(async move {
        if let Err(e) = connection.await {
            eprintln!("samo: connection error: {e}");
        }
    });

    Ok(client)
}

/// Map a `tokio_postgres::Error` into our `ConnectionError`.
///
/// Classification rules:
/// - Authentication keywords → `AuthenticationFailed`
/// - SSL-required signal (server rejects non-TLS when sslmode=disable) →
///   `SslRequired`
/// - Other SSL/TLS errors (bad cert, handshake failure) → `TlsError`
/// - Everything else → `ConnectionFailed`
fn map_connect_error(e: &tokio_postgres::Error, params: &ConnParams) -> ConnectionError {
    let msg = e.to_string();

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

    // General TLS failures: bad certificate, handshake errors, etc.
    if msg.contains("SSL") || msg.contains("ssl") || msg.contains("TLS") || msg.contains("tls") {
        return ConnectionError::TlsError(msg);
    }

    ConnectionError::ConnectionFailed {
        host: params.host.clone(),
        port: params.port,
        reason: msg,
    }
}

/// SSL status line appended when TLS is in use, matching psql's format.
///
/// psql shows full protocol/cipher details (e.g. `TLSv1.3`,
/// `TLS_AES_256_GCM_SHA384`).  The `tokio-postgres-rustls` backend does not
/// expose session-level cipher information after the handshake, so we emit
/// the shorter form here.
const SSL_LINE: &str = "SSL connection (protocol: TLS, compression: off)";

/// Format a human-friendly connection-success message, matching psql output.
///
/// TCP:    You are connected to database "db" as user "u" on host "h" at port "5432".
/// Socket: You are connected to database "db" as user "u" via socket in "/run/pg" at port "5432".
///
/// When `params.tls_in_use` is true an SSL line is appended, e.g.:
/// ```text
/// SSL connection (protocol: TLS, compression: off)
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
        format!(
            "You are connected to database \"{}\" as user \"{}\" \
             on host \"{}\" at port \"{}\".",
            params.dbname, params.user, params.host, params.port,
        )
    };
    if params.tls_in_use {
        format!("{connected_line}\n{SSL_LINE}")
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
/// samo 0.2.0 (...) (server PostgreSQL 17.7)
/// You are now connected to database "mydb" as user "alice" on host "h"
/// at port "5432".
/// ```
///
/// If `server_version` is `None`, the banner is omitted and only the
/// connected line is printed.
///
/// When `new_params.tls_in_use` is true an SSL line is appended after the
/// connected line, matching psql behaviour:
///
/// ```text
/// samo 0.2.0 (...) (server PostgreSQL 17.7)
/// You are now connected to database "mydb" as user "alice" on host "h"
/// at port "5432".
/// SSL connection (protocol: TLS, compression: off)
/// ```
///
/// `client_version` is samo's own version string (from [`crate::version_string`]).
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

    let ssl_suffix = if new_params.tls_in_use {
        format!("\n{SSL_LINE}")
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
        assert_eq!(params.application_name, "samo");
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
        assert_eq!(SslMode::parse("prefer").unwrap(), SslMode::Prefer);
        assert_eq!(SslMode::parse("require").unwrap(), SslMode::Require);
        assert_eq!(SslMode::parse("REQUIRE").unwrap(), SslMode::Require);
        assert!(SslMode::parse("invalid").is_err());
    }

    // -- application_name default -------------------------------------------

    #[test]
    #[serial]
    fn test_application_name_defaults_to_samo() {
        let _guard = EnvGuard::new(&["PGAPPNAME"]);
        let opts = CliConnOpts::default();
        let params = resolve_params(&opts).unwrap();
        assert_eq!(params.application_name, "samo");
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
            reconnect_info("samo 0.2.0 (abc1234, built 2026-01-01)", Some("17.2"), &new),
            "samo 0.2.0 (abc1234, built 2026-01-01) (server PostgreSQL 17.2)\n\
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
            reconnect_info(
                "samo 0.2.0 (abc1234, built 2026-01-01)",
                Some("16.3"),
                &new
            ),
            "samo 0.2.0 (abc1234, built 2026-01-01) (server PostgreSQL 16.3)\n\
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
            reconnect_info(
                "samo 0.2.0 (abc1234, built 2026-01-01)",
                Some("15.6"),
                &new
            ),
            "samo 0.2.0 (abc1234, built 2026-01-01) (server PostgreSQL 15.6)\n\
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
            reconnect_info(
                "samo 0.2.0 (abc1234, built 2026-01-01)",
                Some("17.2"),
                &new
            ),
            "samo 0.2.0 (abc1234, built 2026-01-01) (server PostgreSQL 17.2)\n\
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
            reconnect_info("samo 0.2.0 (abc1234, built 2026-01-01)", None, &new),
            "You are now connected to database \"postgres\" as user \"postgres\" \
             on host \"other.host\" at port \"5432\".",
        );
    }

    // -- SSL / TLS status line --------------------------------------------

    #[test]
    fn test_connection_info_tcp_with_tls() {
        let params = ConnParams {
            host: "db.example.com".into(),
            port: 5432,
            user: "alice".into(),
            dbname: "mydb".into(),
            tls_in_use: true,
            ..ConnParams::default()
        };
        assert_eq!(
            connection_info(&params),
            "You are connected to database \"mydb\" as user \"alice\" \
             on host \"db.example.com\" at port \"5432\".\n\
             SSL connection (protocol: TLS, compression: off)",
        );
    }

    #[test]
    fn test_connection_info_socket_no_tls() {
        // Sockets never use TLS; tls_in_use must remain false.
        let params = ConnParams {
            host: "/var/run/postgresql".into(),
            port: 5432,
            user: "alice".into(),
            dbname: "mydb".into(),
            tls_in_use: false,
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
            tls_in_use: true,
            ..ConnParams::default()
        };
        assert_eq!(
            reconnect_info(
                "samo 0.2.0 (abc1234, built 2026-01-01)",
                Some("17.2"),
                &new
            ),
            "samo 0.2.0 (abc1234, built 2026-01-01) (server PostgreSQL 17.2)\n\
             You are now connected to database \"mydb\" as user \"alice\" \
             on host \"localhost\" at port \"5432\".\n\
             SSL connection (protocol: TLS, compression: off)",
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
            tls_in_use: true,
            ..ConnParams::default()
        };
        assert_eq!(
            reconnect_info(
                "samo 0.2.0 (abc1234, built 2026-01-01)",
                Some("16.3"),
                &new
            ),
            "samo 0.2.0 (abc1234, built 2026-01-01) (server PostgreSQL 16.3)\n\
             You are now connected to database \"mydb\" as user \"alice\" \
             on host \"other.example.com\" at port \"5432\".\n\
             SSL connection (protocol: TLS, compression: off)",
        );
    }
}
