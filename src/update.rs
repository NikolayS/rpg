//! Self-update functionality for the `rpg` binary.
//!
//! Checks for the latest release on GitHub, downloads the matching
//! platform asset, and atomically replaces the running executable.

use std::fmt;
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Deserialize;

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Update-related configuration, nested under `[update]` in `~/.config/rpg/config.toml`.
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct UpdateConfig {
    /// Check for updates automatically on startup (if >24 h since last check).
    pub auto_check: bool,
    /// Release channel to track.
    pub channel: UpdateChannel,
}

/// Release channel for self-updates.
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, Default)]
pub enum UpdateChannel {
    /// Track stable releases only.
    #[default]
    Stable,
    /// Include pre-release / beta builds.
    Beta,
}

// ---------------------------------------------------------------------------
// Release metadata
// ---------------------------------------------------------------------------

/// Information about a published GitHub release.
#[allow(dead_code)]
pub struct ReleaseInfo {
    /// Version string (e.g. `"0.3.0"`), derived from the `tag_name` field.
    pub version: String,
    /// Direct download URL for the platform-matching binary asset.
    pub download_url: String,
    /// ISO 8601 publication timestamp from the GitHub API.
    pub published_at: String,
}

// Raw GitHub Releases API response shape — only the fields we need.
#[derive(Debug, Deserialize)]
struct GhRelease {
    tag_name: String,
    published_at: String,
    assets: Vec<GhAsset>,
}

#[derive(Debug, Deserialize)]
struct GhAsset {
    name: String,
    browser_download_url: String,
}

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors that can occur during a self-update operation.
#[allow(dead_code)]
#[derive(Debug)]
pub enum UpdateError {
    /// A network request failed.
    Http(reqwest::Error),
    /// The GitHub API returned a non-success status code.
    ApiStatus(reqwest::StatusCode),
    /// No release asset matched the current platform.
    NoAssetForPlatform(String),
    /// Could not determine the path of the running executable.
    ExePath(std::io::Error),
    /// A filesystem operation failed.
    Io(std::io::Error),
    /// The version string could not be parsed from the API response.
    BadVersion(String),
    /// Failed to determine or create the cache directory.
    CacheDir,
}

impl fmt::Display for UpdateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Http(e) => write!(f, "HTTP error: {e}"),
            Self::ApiStatus(s) => write!(f, "GitHub API returned status {s}"),
            Self::NoAssetForPlatform(p) => {
                write!(f, "no release asset found for platform: {p}")
            }
            Self::ExePath(e) => write!(f, "could not locate current executable: {e}"),
            Self::Io(e) => write!(f, "I/O error: {e}"),
            Self::BadVersion(s) => write!(f, "unexpected version string: {s}"),
            Self::CacheDir => write!(f, "could not determine cache directory"),
        }
    }
}

impl From<reqwest::Error> for UpdateError {
    fn from(e: reqwest::Error) -> Self {
        Self::Http(e)
    }
}

// ---------------------------------------------------------------------------
// Platform detection
// ---------------------------------------------------------------------------

/// Return the expected asset name for the current OS and architecture.
///
/// Asset names follow the pattern `rpg-<arch>-<vendor>-<os>-<env>`, e.g.
/// `rpg-x86_64-unknown-linux-gnu` or `rpg-aarch64-apple-darwin`.
#[allow(dead_code)]
pub fn platform_asset_name() -> String {
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;

    // Map Rust's consts to the target-triple component conventions used in
    // the release asset filenames produced by CI.
    let triple = match (arch, os) {
        ("x86_64", "linux") => "x86_64-unknown-linux-gnu",
        ("aarch64", "linux") => "aarch64-unknown-linux-gnu",
        ("x86_64", "macos") => "x86_64-apple-darwin",
        ("aarch64", "macos") => "aarch64-apple-darwin",
        ("x86_64", "windows") => "x86_64-pc-windows-msvc",
        ("aarch64", "windows") => "aarch64-pc-windows-msvc",
        (a, o) => return format!("rpg-{a}-unknown-{o}"),
    };

    format!("rpg-{triple}")
}

// ---------------------------------------------------------------------------
// Version checking
// ---------------------------------------------------------------------------

const GITHUB_API_URL: &str = "https://api.github.com/repos/NikolayS/rpg/releases/latest";

/// Query the GitHub Releases API for the latest published release.
///
/// Finds the asset that matches the current platform and returns a
/// [`ReleaseInfo`] containing its download URL and publication timestamp.
#[allow(dead_code)]
pub async fn check_latest_version(client: &reqwest::Client) -> Result<ReleaseInfo, UpdateError> {
    let response = client
        .get(GITHUB_API_URL)
        .header("User-Agent", "rpg")
        .send()
        .await?;

    if !response.status().is_success() {
        return Err(UpdateError::ApiStatus(response.status()));
    }

    let release: GhRelease = response.json().await?;

    // Strip a leading 'v' from the tag name so callers get a plain semver.
    let version = release
        .tag_name
        .strip_prefix('v')
        .unwrap_or(&release.tag_name)
        .to_owned();

    if version.is_empty() {
        return Err(UpdateError::BadVersion(release.tag_name.clone()));
    }

    let asset_name = platform_asset_name();

    let asset = release
        .assets
        .iter()
        .find(|a| a.name == asset_name)
        .ok_or_else(|| UpdateError::NoAssetForPlatform(asset_name.clone()))?;

    Ok(ReleaseInfo {
        version,
        download_url: asset.browser_download_url.clone(),
        published_at: release.published_at,
    })
}

// ---------------------------------------------------------------------------
// Download and replace
// ---------------------------------------------------------------------------

/// Download the binary at `url` and atomically replace the running executable.
///
/// Steps:
/// 1. Stream the download into a temp file adjacent to the current exe.
/// 2. Mark the temp file executable (Unix only).
/// 3. Rename the temp file over the current exe path.
#[allow(dead_code)]
pub async fn download_and_replace(client: &reqwest::Client, url: &str) -> Result<(), UpdateError> {
    use futures::StreamExt;
    use tokio::io::AsyncWriteExt;

    let exe_path = std::env::current_exe().map_err(UpdateError::ExePath)?;

    // Place the temp file in the same directory so rename() is atomic.
    let parent = exe_path
        .parent()
        .ok_or_else(|| UpdateError::Io(std::io::Error::other("exe has no parent directory")))?;

    let tmp_path = parent.join(format!(
        ".rpg-update-{}.tmp",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    ));

    // Stream download into the temp file.
    let response = client.get(url).header("User-Agent", "rpg").send().await?;

    if !response.status().is_success() {
        return Err(UpdateError::ApiStatus(response.status()));
    }

    let mut tmp_file = tokio::fs::File::create(&tmp_path)
        .await
        .map_err(UpdateError::Io)?;

    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let bytes = chunk?;
        tmp_file.write_all(&bytes).await.map_err(UpdateError::Io)?;
    }
    tmp_file.flush().await.map_err(UpdateError::Io)?;
    drop(tmp_file);

    // Set executable bit on Unix.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&tmp_path)
            .map_err(UpdateError::Io)?
            .permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&tmp_path, perms).map_err(UpdateError::Io)?;
    }

    // Atomically replace the running binary.
    fs::rename(&tmp_path, &exe_path).map_err(|e| {
        // Best-effort cleanup on failure.
        let _ = fs::remove_file(&tmp_path);
        UpdateError::Io(e)
    })?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Update-check caching
// ---------------------------------------------------------------------------

/// Return the path used to cache the last-update-check timestamp.
fn update_check_stamp_path() -> Option<PathBuf> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        dirs::config_dir().map(|d| d.join("rpg").join("last_update_check"))
    }
    #[cfg(target_arch = "wasm32")]
    {
        None
    }
}

/// Core logic for [`should_check_update`], testable with an arbitrary path.
fn should_check_update_at(path: &std::path::Path) -> bool {
    let Ok(contents) = fs::read_to_string(path) else {
        return true;
    };

    let last_secs: u64 = match contents.trim().parse() {
        Ok(v) => v,
        Err(_) => return true,
    };

    let now_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    now_secs.saturating_sub(last_secs) > 24 * 3600
}

/// Return `true` if more than 24 hours have passed since the last update check.
///
/// Always returns `true` when the stamp file is absent or unreadable.
#[allow(dead_code)]
pub fn should_check_update() -> bool {
    let Some(path) = update_check_stamp_path() else {
        return true;
    };
    should_check_update_at(&path)
}

/// Core logic for [`record_update_check`], testable with an arbitrary path.
fn record_update_check_at(path: &std::path::Path) {
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }

    let now_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let _ = fs::write(path, now_secs.to_string());
}

/// Write the current Unix timestamp to the update-check stamp file.
///
/// Creates the parent directory if it does not exist.
/// Silently ignores any I/O errors — this is best-effort bookkeeping.
#[allow(dead_code)]
pub fn record_update_check() {
    let Some(path) = update_check_stamp_path() else {
        return;
    };
    record_update_check_at(&path);
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    // -- UpdateError display ------------------------------------------------

    #[test]
    fn update_error_display_api_status() {
        let err = UpdateError::ApiStatus(reqwest::StatusCode::NOT_FOUND);
        assert!(err.to_string().contains("404"));
    }

    #[test]
    fn update_error_display_no_asset() {
        let err = UpdateError::NoAssetForPlatform("rpg-foo-bar".to_owned());
        assert!(err.to_string().contains("rpg-foo-bar"));
    }

    #[test]
    fn update_error_display_bad_version() {
        let err = UpdateError::BadVersion(String::new());
        assert!(err.to_string().contains("unexpected"));
    }

    #[test]
    fn update_error_display_cache_dir() {
        let err = UpdateError::CacheDir;
        assert!(err.to_string().contains("cache directory"));
    }

    #[test]
    fn update_error_display_io() {
        let err = UpdateError::Io(std::io::Error::other("disk full"));
        assert!(err.to_string().contains("disk full"));
    }

    // -- Platform asset name -----------------------------------------------

    #[test]
    fn platform_asset_name_is_non_empty() {
        let name = platform_asset_name();
        assert!(!name.is_empty());
        assert!(name.starts_with("rpg-"));
    }

    #[test]
    fn platform_asset_name_contains_arch() {
        let name = platform_asset_name();
        let arch = std::env::consts::ARCH;
        // The arch should appear somewhere in the asset name.
        assert!(
            name.contains(arch),
            "expected arch {arch:?} in asset name {name:?}"
        );
    }

    // -- UpdateConfig defaults ---------------------------------------------

    #[test]
    fn update_config_defaults() {
        let cfg = UpdateConfig::default();
        assert!(!cfg.auto_check);
        assert!(matches!(cfg.channel, UpdateChannel::Stable));
    }

    // -- Timestamp caching -------------------------------------------------
    //
    // These tests use isolated temp files so they do not share state and
    // can run safely in parallel.

    #[test]
    fn fresh_stamp_suppresses_check() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("last_update_check");

        let now_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        std::fs::write(&path, now_secs.to_string()).expect("write stamp");

        assert!(
            !should_check_update_at(&path),
            "expected should_check_update_at() == false right after writing stamp"
        );
    }

    #[test]
    fn stale_stamp_triggers_check() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("last_update_check");

        // 25 hours in the past.
        let stale_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            .saturating_sub(25 * 3600);
        std::fs::write(&path, stale_secs.to_string()).expect("write stale stamp");

        assert!(
            should_check_update_at(&path),
            "expected should_check_update_at() == true for a 25-hour-old stamp"
        );
    }

    #[test]
    fn missing_stamp_triggers_check() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("last_update_check");
        // File does not exist — should_check_update_at returns true.
        assert!(
            should_check_update_at(&path),
            "expected should_check_update_at() == true when stamp file is absent"
        );
    }

    #[test]
    fn garbage_stamp_triggers_check() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("last_update_check");
        std::fs::write(&path, "not-a-number").expect("write garbage stamp");

        assert!(
            should_check_update_at(&path),
            "expected should_check_update_at() == true for an unparseable stamp"
        );
    }

    #[test]
    fn record_update_check_writes_parseable_timestamp() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("last_update_check");

        record_update_check_at(&path);

        let contents = std::fs::read_to_string(&path).expect("read stamp after record");
        let secs: u64 = contents.trim().parse().expect("parse stamp as u64");
        let now_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        // Stamp should be within 5 seconds of now.
        assert!(
            now_secs.saturating_sub(secs) < 5,
            "stamp timestamp {secs} is too far from now {now_secs}"
        );
    }
}
