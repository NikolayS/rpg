//! SSH tunnel support for Rpg (FR-22).
// SSH tunneling uses russh which is a native-only dependency.
#![cfg(not(target_arch = "wasm32"))]
//!
//! Establishes an SSH connection to a bastion/jump host and forwards a local
//! TCP port to a remote Postgres host through it.  The forwarding loop runs
//! in a background tokio task; callers receive the allocated local port and
//! a [`SshTunnel`] handle they can drop to shut down the tunnel.
//!
//! # Usage
//!
//! ```text
//! local 127.0.0.1:<allocated_port>  →  SSH bastion  →  remote host:5432
//! ```
//!
//! Call [`open_tunnel`] with an [`SshTunnelConfig`] (from `config.rs`) and
//! the target `remote_host`/`remote_port`.  Keep the returned [`SshTunnel`]
//! alive for the lifetime of the Postgres connection; dropping it aborts the
//! forwarding task.

use std::path::PathBuf;
use std::sync::Arc;

use russh::client::{self, AuthResult, Handler};
use russh::keys::{decode_secret_key, ssh_key};

pub use crate::config::SshTunnelConfig;

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors that can occur when establishing or running an SSH tunnel.
#[derive(Debug, thiserror::Error)]
#[allow(dead_code)]
pub enum SshTunnelError {
    #[error("SSH tunnel: failed to connect to {host}:{port}: {reason}")]
    Connect {
        host: String,
        port: u16,
        reason: String,
    },

    #[error("SSH tunnel: authentication failed for user \"{user}\" on {host}")]
    AuthFailed { user: String, host: String },

    #[error("SSH tunnel: could not read private key file \"{path}\": {reason}")]
    KeyFile { path: String, reason: String },

    #[error("SSH tunnel: could not bind local listener: {0}")]
    BindFailed(String),

    #[error("SSH tunnel: could not open TCP forwarding channel: {0}")]
    ChannelOpenFailed(String),

    /// The server presented a host key that differs from the recorded one.
    ///
    /// This is a strong indicator of a MITM attack or a server key rotation.
    /// The user must manually remove the stale entry from `~/.ssh/known_hosts`
    /// before reconnecting.
    #[error(
        "SSH tunnel: host key mismatch for {host}:{port} \
         (recorded key at line {line} differs from server key — \
         possible MITM attack or server key rotation; \
         remove the stale entry from ~/.ssh/known_hosts to continue)"
    )]
    HostKeyMismatch {
        host: String,
        port: u16,
        line: usize,
    },

    /// The server's host key is not in `~/.ssh/known_hosts` and strict mode
    /// is enabled.
    #[error(
        "SSH tunnel: unknown host key for {host}:{port} \
         (strict host key checking is enabled; \
         add the host key to ~/.ssh/known_hosts or set \
         strict_host_key_checking = false to allow TOFU)"
    )]
    UnknownHost { host: String, port: u16 },

    #[error("SSH tunnel: {0}")]
    Other(String),
}

impl From<russh::Error> for SshTunnelError {
    fn from(e: russh::Error) -> Self {
        Self::Other(e.to_string())
    }
}

// ---------------------------------------------------------------------------
// Spec parser (for CLI --ssh-tunnel user@host:port)
// ---------------------------------------------------------------------------

/// Parsed form of a `user@host:port` CLI argument.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshTunnelSpec {
    pub user: String,
    pub host: String,
    pub port: u16,
}

impl SshTunnelSpec {
    /// Parse `user@host:port` or `user@host` (port defaults to 22).
    ///
    /// Returns `None` when the string does not match the expected format.
    pub fn parse(s: &str) -> Option<Self> {
        // Must contain '@' separating user from host.
        let (user, rest) = s.split_once('@')?;
        if user.is_empty() {
            return None;
        }
        // Optional ':port' at the end.
        let (host, port) = if let Some((h, p)) = rest.rsplit_once(':') {
            let port = p.parse::<u16>().ok()?;
            (h, port)
        } else {
            (rest, 22)
        };
        if host.is_empty() {
            return None;
        }
        Some(Self {
            user: user.to_owned(),
            host: host.to_owned(),
            port,
        })
    }
}

impl From<SshTunnelSpec> for SshTunnelConfig {
    fn from(spec: SshTunnelSpec) -> Self {
        Self {
            host: spec.host,
            port: spec.port,
            user: spec.user,
            ..Default::default()
        }
    }
}

// ---------------------------------------------------------------------------
// known_hosts verification
// ---------------------------------------------------------------------------

/// Outcome of checking the server key against `~/.ssh/known_hosts`.
#[derive(Debug)]
pub enum HostKeyStatus {
    /// Key matched a recorded entry — connection is trusted.
    Trusted,
    /// Host was unknown; key has been recorded (TOFU).
    RecordedNew,
    /// Key does not match the recorded entry at `line`.
    Mismatch { line: usize },
    /// Host key verification could not be performed (I/O error or no home
    /// directory); the tunnel proceeds with a warning.
    Unavailable(String),
}

/// Verify `server_key` for `host`:`port` against `~/.ssh/known_hosts`.
///
/// - Returns [`HostKeyStatus::Trusted`] when a matching entry is found.
/// - Returns [`HostKeyStatus::Mismatch`] when a conflicting entry is found.
/// - Returns [`HostKeyStatus::RecordedNew`] when no entry exists and the key
///   has been appended (TOFU).
/// - Returns [`HostKeyStatus::Unavailable`] on I/O or environment errors.
pub fn verify_or_learn_host_key(
    host: &str,
    port: u16,
    server_key: &ssh_key::PublicKey,
) -> HostKeyStatus {
    use russh::keys::known_hosts as kh;

    match kh::check_known_hosts(host, port, server_key) {
        Ok(true) => HostKeyStatus::Trusted,
        Ok(false) => {
            // Host not found — record it (TOFU).
            match kh::learn_known_hosts(host, port, server_key) {
                Ok(()) => HostKeyStatus::RecordedNew,
                Err(e) => HostKeyStatus::Unavailable(e.to_string()),
            }
        }
        Err(russh::keys::Error::KeyChanged { line }) => HostKeyStatus::Mismatch { line },
        Err(russh::keys::Error::NoHomeDir) => {
            HostKeyStatus::Unavailable("no home directory found".to_owned())
        }
        Err(e) => HostKeyStatus::Unavailable(e.to_string()),
    }
}

// ---------------------------------------------------------------------------
// Russh Handler implementation
// ---------------------------------------------------------------------------

/// Russh `Handler` that verifies the server host key against
/// `~/.ssh/known_hosts` during the SSH handshake.
///
/// Behaviour is controlled by `strict`:
///
/// | strict | unknown host | key mismatch |
/// |--------|-------------|--------------|
/// | true   | reject      | reject       |
/// | false  | TOFU + warn | reject       |
struct TunnelHandler {
    host: String,
    port: u16,
    strict: bool,
}

impl Handler for TunnelHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &ssh_key::PublicKey,
    ) -> Result<bool, Self::Error> {
        match verify_or_learn_host_key(&self.host, self.port, server_public_key) {
            HostKeyStatus::Trusted => Ok(true),

            HostKeyStatus::RecordedNew => {
                if self.strict {
                    eprintln!(
                        "WARNING: SSH host key for {}:{} is not in known_hosts \
                         and strict_host_key_checking is enabled. \
                         Refusing connection.",
                        self.host, self.port
                    );
                    Ok(false)
                } else {
                    eprintln!(
                        "WARNING: Permanently added {}:{} to the list of \
                         known hosts (TOFU).",
                        self.host, self.port
                    );
                    Ok(true)
                }
            }

            HostKeyStatus::Mismatch { line } => {
                eprintln!(
                    "WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED!\n\
                     SSH tunnel: host key for {}:{} differs from the \
                     key recorded at line {} of ~/.ssh/known_hosts.\n\
                     This may indicate a MITM attack or server key rotation.\n\
                     Remove the offending entry from ~/.ssh/known_hosts \
                     and reconnect.",
                    self.host, self.port, line
                );
                // Always reject on mismatch regardless of strict mode.
                Ok(false)
            }

            HostKeyStatus::Unavailable(reason) => {
                eprintln!(
                    "WARNING: SSH host key verification unavailable \
                     for {}:{}: {}. Proceeding without verification.",
                    self.host, self.port, reason
                );
                // Proceed: we cannot verify, but we shouldn't block on
                // transient environment errors (e.g., missing ~/.ssh dir on
                // first run).
                Ok(true)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tunnel handle
// ---------------------------------------------------------------------------

/// A live SSH tunnel.
///
/// The forwarding loop continues until this handle is dropped.  Dropping it
/// sends a shutdown signal to the background task.
pub struct SshTunnel {
    /// Local port that forwards to the remote Postgres instance.
    pub local_port: u16,
    /// Abort handle for the background forwarding task.
    abort: tokio::task::AbortHandle,
}

impl Drop for SshTunnel {
    fn drop(&mut self) {
        self.abort.abort();
    }
}

// ---------------------------------------------------------------------------
// Key loading helpers
// ---------------------------------------------------------------------------

/// Expand `~` at the start of a path to the user's home directory.
fn expand_tilde(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(path)
}

/// Default SSH private key paths to try when no explicit key is configured.
fn default_key_paths() -> Vec<PathBuf> {
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };
    vec![
        home.join(".ssh").join("id_ed25519"),
        home.join(".ssh").join("id_rsa"),
    ]
}

/// Try to load a private key from `path`.  Returns `None` when the file does
/// not exist; returns an error for other failures.
fn try_load_key(path: &PathBuf) -> Result<Option<russh::keys::PrivateKey>, SshTunnelError> {
    if !path.exists() {
        return Ok(None);
    }
    let pem = std::fs::read_to_string(path).map_err(|e| SshTunnelError::KeyFile {
        path: path.display().to_string(),
        reason: e.to_string(),
    })?;
    // Try without a passphrase first (passphrase-protected keys require
    // interactive prompting — not implemented here; skip and try the next).
    match decode_secret_key(&pem, None) {
        Ok(key) => Ok(Some(key)),
        Err(_) => Ok(None),
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Establish an SSH tunnel and start the local port-forwarding listener.
///
/// Returns an [`SshTunnel`] whose `local_port` field contains the allocated
/// local port.  Connect Postgres to `127.0.0.1:local_port`.
///
/// The `remote_host` and `remote_port` describe where the connection is
/// forwarded *on the remote side* (i.e. the Postgres server as seen from
/// the bastion).
pub async fn open_tunnel(
    cfg: &SshTunnelConfig,
    remote_host: &str,
    remote_port: u16,
) -> Result<SshTunnel, SshTunnelError> {
    // 1. Connect to the bastion SSH server.
    let ssh_addr = format!("{}:{}", cfg.host, cfg.port);
    let ssh_config = Arc::new(client::Config {
        keepalive_interval: Some(std::time::Duration::from_secs(60)),
        keepalive_max: 5,
        ..Default::default()
    });

    let handler = TunnelHandler {
        host: cfg.host.clone(),
        port: cfg.port,
        strict: cfg.strict_host_key_checking,
    };

    let connect_future = client::connect(ssh_config, ssh_addr.as_str(), handler);
    let connect_timeout = std::time::Duration::from_secs(cfg.connect_timeout);
    let mut handle = tokio::time::timeout(connect_timeout, connect_future)
        .await
        .map_err(|_| SshTunnelError::Connect {
            host: cfg.host.clone(),
            port: cfg.port,
            reason: format!("connection timed out after {}s", cfg.connect_timeout),
        })?
        .map_err(|e| SshTunnelError::Connect {
            host: cfg.host.clone(),
            port: cfg.port,
            reason: e.to_string(),
        })?;

    // 2. Authenticate.
    let authed = try_authenticate(&mut handle, cfg).await?;
    if !authed {
        return Err(SshTunnelError::AuthFailed {
            user: cfg.user.clone(),
            host: cfg.host.clone(),
        });
    }

    // 3. Bind local listener on an OS-assigned port.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|e| SshTunnelError::BindFailed(e.to_string()))?;
    let local_port = listener
        .local_addr()
        .map_err(|e| SshTunnelError::BindFailed(e.to_string()))?
        .port();

    // 4. Spawn background forwarding task.
    let remote_host = remote_host.to_owned();
    let task = tokio::spawn(async move {
        forwarding_loop(listener, handle, remote_host, u32::from(remote_port)).await;
    });

    Ok(SshTunnel {
        local_port,
        abort: task.abort_handle(),
    })
}

// ---------------------------------------------------------------------------
// Authentication helpers
// ---------------------------------------------------------------------------

/// Attempt SSH authentication: key-based first, then password.
/// Returns `true` on success.
async fn try_authenticate(
    handle: &mut client::Handle<TunnelHandler>,
    cfg: &SshTunnelConfig,
) -> Result<bool, SshTunnelError> {
    let key_paths: Vec<PathBuf> = if let Some(ref k) = cfg.key {
        vec![expand_tilde(k)]
    } else {
        default_key_paths()
    };

    for path in &key_paths {
        let Some(key) = try_load_key(path)? else {
            continue;
        };
        let pk_with_hash = russh::keys::key::PrivateKeyWithHashAlg::new(
            Arc::new(key),
            None, // let russh choose the hash algorithm
        );
        match handle
            .authenticate_publickey(cfg.user.clone(), pk_with_hash)
            .await
        {
            Ok(AuthResult::Success) => return Ok(true),
            Ok(AuthResult::Failure { .. }) => {
                // This key was rejected; try the next one.
            }
            Err(e) => {
                return Err(SshTunnelError::Other(format!("public-key auth error: {e}")));
            }
        }
    }

    // Fallback: password authentication (never logged).
    if let Some(ref pw) = cfg.password {
        match handle
            .authenticate_password(cfg.user.clone(), pw.clone())
            .await
        {
            Ok(AuthResult::Success) => return Ok(true),
            Ok(AuthResult::Failure { .. }) => {}
            Err(e) => {
                return Err(SshTunnelError::Other(format!("password auth error: {e}")));
            }
        }
    }

    Ok(false)
}

// ---------------------------------------------------------------------------
// Forwarding loop
// ---------------------------------------------------------------------------

/// Accept local connections and forward each one through the SSH tunnel.
async fn forwarding_loop(
    listener: tokio::net::TcpListener,
    handle: client::Handle<TunnelHandler>,
    remote_host: String,
    remote_port: u32,
) {
    loop {
        let Ok((local_stream, _peer)) = listener.accept().await else {
            break;
        };
        let rh = remote_host.clone();
        let ch_result = handle
            .channel_open_direct_tcpip(rh.as_str(), remote_port, "127.0.0.1", 0)
            .await;
        match ch_result {
            Ok(channel) => {
                tokio::spawn(proxy_connection(local_stream, channel));
            }
            Err(e) => {
                crate::logging::info(
                    "ssh_tunnel",
                    &format!("direct-tcpip channel open failed: {e}"),
                );
            }
        }
    }
}

/// Bidirectionally copy data between a local TCP stream and an SSH channel.
async fn proxy_connection(
    mut local: tokio::net::TcpStream,
    channel: russh::Channel<russh::client::Msg>,
) {
    let mut stream = channel.into_stream();
    let _result = tokio::io::copy_bidirectional(&mut local, &mut stream).await;
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- SshTunnelSpec::parse ------------------------------------------------

    #[test]
    fn parse_user_at_host_port() {
        let spec = SshTunnelSpec::parse("deploy@bastion.example.com:2222").unwrap();
        assert_eq!(spec.user, "deploy");
        assert_eq!(spec.host, "bastion.example.com");
        assert_eq!(spec.port, 2222);
    }

    #[test]
    fn parse_user_at_host_default_port() {
        let spec = SshTunnelSpec::parse("alice@jumphost.internal").unwrap();
        assert_eq!(spec.user, "alice");
        assert_eq!(spec.host, "jumphost.internal");
        assert_eq!(spec.port, 22);
    }

    #[test]
    fn parse_missing_at_sign_returns_none() {
        assert!(SshTunnelSpec::parse("justhost:22").is_none());
    }

    #[test]
    fn parse_empty_user_returns_none() {
        assert!(SshTunnelSpec::parse("@host:22").is_none());
    }

    #[test]
    fn parse_empty_host_returns_none() {
        assert!(SshTunnelSpec::parse("user@:22").is_none());
    }

    #[test]
    fn parse_invalid_port_returns_none() {
        assert!(SshTunnelSpec::parse("user@host:notaport").is_none());
    }

    #[test]
    fn parse_port_out_of_range_returns_none() {
        assert!(SshTunnelSpec::parse("user@host:99999").is_none());
    }

    #[test]
    fn spec_into_config() {
        let spec = SshTunnelSpec {
            user: "deploy".into(),
            host: "bastion.example.com".into(),
            port: 22,
        };
        let cfg: SshTunnelConfig = spec.into();
        assert_eq!(cfg.user, "deploy");
        assert_eq!(cfg.host, "bastion.example.com");
        assert_eq!(cfg.port, 22);
        assert!(cfg.key.is_none());
        assert!(cfg.password.is_none());
    }

    // -- known_hosts parsing -------------------------------------------------

    /// Helper: write a `known_hosts` file in a temp dir and return the path.
    fn write_known_hosts(dir: &tempfile::TempDir, content: &str) -> PathBuf {
        let path = dir.path().join("known_hosts");
        std::fs::write(&path, content).unwrap();
        path
    }

    /// A real Ed25519 public key in OpenSSH wire format (base64).
    ///
    /// Generated with: `ssh-keygen -t ed25519 -N "" -f /tmp/test_key`
    /// and extracted from the resulting `.pub` file.
    const TEST_KEY_B64: &str =
        "AAAAC3NzaC1lZDI1NTE5AAAAIJdD7y3aLq454yWBdwLWbieU1ebz9/cu7/QEXn9OIeZJ";

    fn test_pubkey() -> ssh_key::PublicKey {
        russh::keys::parse_public_key_base64(TEST_KEY_B64).unwrap()
    }

    #[test]
    fn known_hosts_match_plain_hostname() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_known_hosts(&dir, &format!("example.com ssh-ed25519 {TEST_KEY_B64}\n"));

        let result =
            russh::keys::check_known_hosts_path("example.com", 22, &test_pubkey(), &path).unwrap();
        assert!(result, "key should match the plain hostname entry");
    }

    #[test]
    fn known_hosts_match_non_standard_port() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_known_hosts(
            &dir,
            &format!("[example.com]:2222 ssh-ed25519 {TEST_KEY_B64}\n"),
        );

        let result =
            russh::keys::check_known_hosts_path("example.com", 2222, &test_pubkey(), &path)
                .unwrap();
        assert!(result, "key should match the bracketed host:port entry");
    }

    #[test]
    fn known_hosts_no_match_different_host() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_known_hosts(
            &dir,
            &format!("other.example.com ssh-ed25519 {TEST_KEY_B64}\n"),
        );

        let result =
            russh::keys::check_known_hosts_path("example.com", 22, &test_pubkey(), &path).unwrap();
        assert!(!result, "should not match a different hostname");
    }

    #[test]
    fn known_hosts_key_mismatch_returns_error() {
        // A different Ed25519 key (pijul.org's key from russh's own tests).
        const OTHER_KEY_B64: &str =
            "AAAAC3NzaC1lZDI1NTE5AAAAIA6rWI3G1sz07DnfFlrouTcysQlj2P+jpNSOEWD9OJ3X";

        let dir = tempfile::tempdir().unwrap();
        // File contains TEST_KEY_B64 but we'll check with OTHER_KEY_B64.
        let path = write_known_hosts(&dir, &format!("example.com ssh-ed25519 {TEST_KEY_B64}\n"));

        let other_key = russh::keys::parse_public_key_base64(OTHER_KEY_B64).unwrap();
        let err =
            russh::keys::check_known_hosts_path("example.com", 22, &other_key, &path).unwrap_err();

        // Must be a KeyChanged error.
        assert!(
            matches!(err, russh::keys::Error::KeyChanged { line: 1 }),
            "expected KeyChanged at line 1, got: {err:?}",
        );
    }

    #[test]
    fn known_hosts_hashed_hostname_match() {
        // This hashed entry encodes "example.com" with a known salt and hash
        // taken from the russh known_hosts test suite.
        // |1|O33ESRMWPVkMYIwJ1Uw+n877jTo=|... corresponds to "example.com".
        const HASHED_ENTRY_KEY: &str =
            "AAAAC3NzaC1lZDI1NTE5AAAAILIG2T/B0l0gaqj3puu510tu9N1OkQ4znY3LYuEm5zCF";
        const HASHED_HOST: &str = "|1|O33ESRMWPVkMYIwJ1Uw+n877jTo=|nuuC5vEqXlEZ/8BXQR7m619W6Ak=";

        let dir = tempfile::tempdir().unwrap();
        let path = write_known_hosts(
            &dir,
            &format!("{HASHED_HOST} ssh-ed25519 {HASHED_ENTRY_KEY}\n"),
        );

        let key = russh::keys::parse_public_key_base64(HASHED_ENTRY_KEY).unwrap();
        let result = russh::keys::check_known_hosts_path("example.com", 22, &key, &path).unwrap();
        assert!(result, "hashed hostname should match 'example.com'");
    }

    #[test]
    fn known_hosts_hashed_hostname_no_match_different_host() {
        const HASHED_ENTRY_KEY: &str =
            "AAAAC3NzaC1lZDI1NTE5AAAAILIG2T/B0l0gaqj3puu510tu9N1OkQ4znY3LYuEm5zCF";
        const HASHED_HOST: &str = "|1|O33ESRMWPVkMYIwJ1Uw+n877jTo=|nuuC5vEqXlEZ/8BXQR7m619W6Ak=";

        let dir = tempfile::tempdir().unwrap();
        let path = write_known_hosts(
            &dir,
            &format!("{HASHED_HOST} ssh-ed25519 {HASHED_ENTRY_KEY}\n"),
        );

        let key = russh::keys::parse_public_key_base64(HASHED_ENTRY_KEY).unwrap();
        // "other.example.com" should NOT match the hashed "example.com" entry.
        let result =
            russh::keys::check_known_hosts_path("other.example.com", 22, &key, &path).unwrap();
        assert!(!result, "hashed hostname should not match a different host");
    }

    #[test]
    fn known_hosts_empty_file_returns_false() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_known_hosts(&dir, "");

        let result =
            russh::keys::check_known_hosts_path("example.com", 22, &test_pubkey(), &path).unwrap();
        assert!(!result, "empty known_hosts should return false (unknown)");
    }

    #[test]
    fn known_hosts_comment_lines_ignored() {
        let dir = tempfile::tempdir().unwrap();
        // The key is only in a comment — should NOT match.
        let path = write_known_hosts(&dir, &format!("# example.com ssh-ed25519 {TEST_KEY_B64}\n"));

        let result =
            russh::keys::check_known_hosts_path("example.com", 22, &test_pubkey(), &path).unwrap();
        assert!(!result, "commented-out entries must not match");
    }

    // -- verify_or_learn_host_key (unit-testable path variant) ---------------

    /// A thin wrapper that accepts a custom path, mirroring
    /// `verify_or_learn_host_key` but bypassing the real `~/.ssh/known_hosts`.
    fn verify_or_learn_at_path(
        host: &str,
        port: u16,
        server_key: &ssh_key::PublicKey,
        path: &PathBuf,
    ) -> HostKeyStatus {
        use russh::keys::known_hosts as kh;

        match kh::check_known_hosts_path(host, port, server_key, path) {
            Ok(true) => HostKeyStatus::Trusted,
            Ok(false) => match kh::learn_known_hosts_path(host, port, server_key, path) {
                Ok(()) => HostKeyStatus::RecordedNew,
                Err(e) => HostKeyStatus::Unavailable(e.to_string()),
            },
            Err(russh::keys::Error::KeyChanged { line }) => HostKeyStatus::Mismatch { line },
            Err(russh::keys::Error::NoHomeDir) => {
                HostKeyStatus::Unavailable("no home directory found".to_owned())
            }
            Err(e) => HostKeyStatus::Unavailable(e.to_string()),
        }
    }

    #[test]
    fn verify_trusted_when_key_matches() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_known_hosts(&dir, &format!("example.com ssh-ed25519 {TEST_KEY_B64}\n"));
        let status = verify_or_learn_at_path("example.com", 22, &test_pubkey(), &path);
        assert!(matches!(status, HostKeyStatus::Trusted));
    }

    #[test]
    fn verify_records_new_unknown_host() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_known_hosts(&dir, "");
        let status = verify_or_learn_at_path("newhost.example.com", 22, &test_pubkey(), &path);
        assert!(
            matches!(status, HostKeyStatus::RecordedNew),
            "unknown host should be recorded (TOFU)"
        );

        // Verify the key was actually written.
        let written = std::fs::read_to_string(&path).unwrap();
        assert!(
            written.contains("newhost.example.com"),
            "known_hosts file should contain the new hostname"
        );
    }

    #[test]
    fn verify_mismatch_on_changed_key() {
        const OTHER_KEY_B64: &str =
            "AAAAC3NzaC1lZDI1NTE5AAAAIA6rWI3G1sz07DnfFlrouTcysQlj2P+jpNSOEWD9OJ3X";

        let dir = tempfile::tempdir().unwrap();
        let path = write_known_hosts(&dir, &format!("example.com ssh-ed25519 {TEST_KEY_B64}\n"));

        let other_key = russh::keys::parse_public_key_base64(OTHER_KEY_B64).unwrap();
        let status = verify_or_learn_at_path("example.com", 22, &other_key, &path);
        assert!(
            matches!(status, HostKeyStatus::Mismatch { line: 1 }),
            "changed key should produce Mismatch, got: {status:?}"
        );
    }

    // -- SshTunnelConfig defaults --------------------------------------------

    #[test]
    fn config_default_strict_host_key_checking_is_true() {
        let cfg = SshTunnelConfig {
            host: "bastion.example.com".into(),
            port: 22,
            user: "deploy".into(),
            ..Default::default()
        };
        assert!(
            cfg.strict_host_key_checking,
            "strict_host_key_checking should default to true"
        );
    }

    #[test]
    fn config_default_connect_timeout_is_15() {
        let cfg = SshTunnelConfig {
            host: "bastion.example.com".into(),
            port: 22,
            user: "deploy".into(),
            ..Default::default()
        };
        assert_eq!(
            cfg.connect_timeout, 15,
            "connect_timeout should default to 15 seconds"
        );
    }

    #[test]
    fn config_custom_connect_timeout_parsed_from_toml() {
        let toml_str = r#"
host = "bastion.example.com"
port = 22
user = "deploy"
connect_timeout = 30
"#;
        let cfg: SshTunnelConfig = toml::from_str(toml_str).expect("should parse SshTunnelConfig");
        assert_eq!(cfg.connect_timeout, 30);
    }
}
