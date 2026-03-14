//! Bidirectional issue sync manager (Phase 4).
//!
//! Tracks mappings between local `PostgresAI` issue IDs and remote
//! connector issue IDs (GitHub, GitLab, Jira, etc.) and orchestrates
//! cross-connector issue creation and updates.

use std::time::SystemTime;

use super::{ConnectorError, IssueId, IssueRequest};

// ---------------------------------------------------------------------------
// SyncDirection
// ---------------------------------------------------------------------------

/// Direction of issue synchronisation between connectors.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncDirection {
    /// Local (`PostgresAI`) → remote connector.
    Outbound,
    /// Remote connector → local (`PostgresAI`).
    Inbound,
    /// Issues are kept in sync in both directions.
    Bidirectional,
}

impl std::fmt::Display for SyncDirection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Outbound => write!(f, "outbound"),
            Self::Inbound => write!(f, "inbound"),
            Self::Bidirectional => write!(f, "bidirectional"),
        }
    }
}

// ---------------------------------------------------------------------------
// SyncRecord
// ---------------------------------------------------------------------------

/// A single sync mapping between a local and a remote issue.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct SyncRecord {
    /// `PostgresAI` issue ID (local side).
    pub local_id: String,
    /// GitHub / Jira / GitLab issue ID (remote side).
    pub remote_id: String,
    /// Identifies the connector that owns the remote issue.
    pub connector_id: String,
    /// Wall-clock time of the most recent successful sync.
    pub last_synced: SystemTime,
    /// Direction in which this sync pair operates.
    pub sync_direction: SyncDirection,
}

// ---------------------------------------------------------------------------
// IssueSyncManager
// ---------------------------------------------------------------------------

/// Manages sync state between the local `PostgresAI` tracker and remote
/// issue connectors.
///
/// The manager keeps an in-memory registry of [`SyncRecord`]s and
/// provides lookup and registration helpers. Actual HTTP calls are
/// delegated to connector implementations via the [`super::Connector`]
/// trait; this struct only handles the bookkeeping side.
#[allow(dead_code)]
pub struct IssueSyncManager {
    records: Vec<SyncRecord>,
}

#[allow(dead_code)]
impl IssueSyncManager {
    /// Create an empty sync manager.
    pub fn new() -> Self {
        Self {
            records: Vec::new(),
        }
    }

    /// Return all sync records currently tracked.
    pub fn records(&self) -> &[SyncRecord] {
        &self.records
    }

    /// Register a new sync pair.
    ///
    /// If a record with the same `(local_id, connector_id)` already
    /// exists it is replaced; otherwise a new record is appended.
    /// The direction defaults to [`SyncDirection::Bidirectional`].
    pub fn register_sync(
        &mut self,
        local_id: impl Into<String>,
        remote_id: impl Into<String>,
        connector_id: impl Into<String>,
    ) -> &SyncRecord {
        let local_id = local_id.into();
        let remote_id = remote_id.into();
        let connector_id = connector_id.into();

        let record = SyncRecord {
            local_id: local_id.clone(),
            remote_id,
            connector_id: connector_id.clone(),
            last_synced: SystemTime::now(),
            sync_direction: SyncDirection::Bidirectional,
        };

        if let Some(pos) = self
            .records
            .iter()
            .position(|r| r.local_id == local_id && r.connector_id == connector_id)
        {
            self.records[pos] = record;
            &self.records[pos]
        } else {
            self.records.push(record);
            self.records.last().expect("just pushed")
        }
    }

    /// Look up the remote ID for a local issue on a specific connector.
    ///
    /// Returns `None` if no sync record exists for the pair.
    pub fn find_remote_id(&self, local_id: &str, connector_id: &str) -> Option<&str> {
        self.records
            .iter()
            .find(|r| r.local_id == local_id && r.connector_id == connector_id)
            .map(|r| r.remote_id.as_str())
    }

    /// Look up the local ID for a remote issue from a specific connector.
    ///
    /// Returns `None` if no sync record exists for the pair.
    pub fn find_local_id(&self, remote_id: &str, connector_id: &str) -> Option<&str> {
        self.records
            .iter()
            .find(|r| r.remote_id == remote_id && r.connector_id == connector_id)
            .map(|r| r.local_id.as_str())
    }

    /// Create or update an issue on the target connector and record the
    /// mapping.
    ///
    /// # Behaviour
    ///
    /// 1. Check whether a sync record already exists for
    ///    `(source_connector, target_connector, issue_request.title)`.
    ///    This implementation uses `target_connector` as the
    ///    `connector_id` key and derives a stable `local_id` from
    ///    `source_connector + ":" + issue_request.title`.
    /// 2. If a remote ID already exists, re-use it; otherwise call
    ///    `target_connector.create_issue` to obtain one.
    /// 3. Register (or refresh) the sync mapping and return the record.
    ///
    /// No actual network calls are made — the `target_connector`
    /// parameter is an opaque function that stands in for a real
    /// connector's `create_issue` implementation so that the manager
    /// can be tested without an HTTP client.
    pub fn sync_issue(
        &mut self,
        source_connector: &str,
        target_connector: &str,
        issue_request: &IssueRequest,
        create_fn: impl FnOnce(&IssueRequest) -> Result<IssueId, ConnectorError>,
    ) -> Result<SyncRecord, ConnectorError> {
        let local_id = format!("{}:{}", source_connector, issue_request.title);

        let remote_id = if let Some(existing) = self.find_remote_id(&local_id, target_connector) {
            existing.to_string()
        } else {
            create_fn(issue_request)?
        };

        let record = SyncRecord {
            local_id: local_id.clone(),
            remote_id: remote_id.clone(),
            connector_id: target_connector.to_string(),
            last_synced: SystemTime::now(),
            sync_direction: SyncDirection::Outbound,
        };

        if let Some(pos) = self
            .records
            .iter()
            .position(|r| r.local_id == local_id && r.connector_id == target_connector)
        {
            self.records[pos] = record.clone();
        } else {
            self.records.push(record.clone());
        }

        Ok(record)
    }
}

impl Default for IssueSyncManager {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::connectors::IssueRequest;

    fn make_request(title: &str) -> IssueRequest {
        IssueRequest {
            title: title.to_string(),
            body: "test body".to_string(),
            labels: vec![],
            assignees: vec![],
            metadata: HashMap::new(),
        }
    }

    // ------------------------------------------------------------------
    // SyncDirection tests
    // ------------------------------------------------------------------

    #[test]
    fn sync_direction_display_outbound() {
        assert_eq!(SyncDirection::Outbound.to_string(), "outbound");
    }

    #[test]
    fn sync_direction_display_inbound() {
        assert_eq!(SyncDirection::Inbound.to_string(), "inbound");
    }

    #[test]
    fn sync_direction_display_bidirectional() {
        assert_eq!(SyncDirection::Bidirectional.to_string(), "bidirectional");
    }

    #[test]
    fn sync_direction_equality() {
        assert_eq!(SyncDirection::Outbound, SyncDirection::Outbound);
        assert_ne!(SyncDirection::Outbound, SyncDirection::Inbound);
        assert_ne!(SyncDirection::Inbound, SyncDirection::Bidirectional);
    }

    // ------------------------------------------------------------------
    // IssueSyncManager — new / default
    // ------------------------------------------------------------------

    #[test]
    fn new_manager_is_empty() {
        let mgr = IssueSyncManager::new();
        assert!(mgr.records().is_empty());
    }

    #[test]
    fn default_manager_is_empty() {
        let mgr = IssueSyncManager::default();
        assert!(mgr.records().is_empty());
    }

    // ------------------------------------------------------------------
    // register_sync
    // ------------------------------------------------------------------

    #[test]
    fn register_sync_adds_record() {
        let mut mgr = IssueSyncManager::new();
        mgr.register_sync("local-1", "remote-gh-42", "github");
        assert_eq!(mgr.records().len(), 1);
        let r = &mgr.records()[0];
        assert_eq!(r.local_id, "local-1");
        assert_eq!(r.remote_id, "remote-gh-42");
        assert_eq!(r.connector_id, "github");
        assert_eq!(r.sync_direction, SyncDirection::Bidirectional);
    }

    #[test]
    fn register_sync_updates_existing_record() {
        let mut mgr = IssueSyncManager::new();
        mgr.register_sync("local-1", "remote-old", "github");
        mgr.register_sync("local-1", "remote-new", "github");
        // Should have replaced in-place, not appended.
        assert_eq!(mgr.records().len(), 1);
        assert_eq!(mgr.records()[0].remote_id, "remote-new");
    }

    #[test]
    fn register_sync_different_connectors_are_separate_records() {
        let mut mgr = IssueSyncManager::new();
        mgr.register_sync("local-1", "gh-100", "github");
        mgr.register_sync("local-1", "jira-ABC-1", "jira");
        assert_eq!(mgr.records().len(), 2);
    }

    // ------------------------------------------------------------------
    // find_remote_id / find_local_id
    // ------------------------------------------------------------------

    #[test]
    fn find_remote_id_returns_some_when_registered() {
        let mut mgr = IssueSyncManager::new();
        mgr.register_sync("loc-42", "rem-99", "gitlab");
        assert_eq!(mgr.find_remote_id("loc-42", "gitlab"), Some("rem-99"));
    }

    #[test]
    fn find_remote_id_returns_none_for_unknown_local() {
        let mgr = IssueSyncManager::new();
        assert!(mgr.find_remote_id("nope", "github").is_none());
    }

    #[test]
    fn find_remote_id_returns_none_for_wrong_connector() {
        let mut mgr = IssueSyncManager::new();
        mgr.register_sync("loc-1", "rem-1", "github");
        assert!(mgr.find_remote_id("loc-1", "jira").is_none());
    }

    #[test]
    fn find_local_id_returns_some_when_registered() {
        let mut mgr = IssueSyncManager::new();
        mgr.register_sync("loc-7", "rem-7", "github");
        assert_eq!(mgr.find_local_id("rem-7", "github"), Some("loc-7"));
    }

    #[test]
    fn find_local_id_returns_none_for_unknown_remote() {
        let mgr = IssueSyncManager::new();
        assert!(mgr.find_local_id("no-such-remote", "github").is_none());
    }

    // ------------------------------------------------------------------
    // sync_issue
    // ------------------------------------------------------------------

    #[test]
    fn sync_issue_calls_create_fn_when_no_existing_record() {
        let mut mgr = IssueSyncManager::new();
        let req = make_request("High CPU usage");
        let result = mgr.sync_issue("postgresai", "github", &req, |_| {
            Ok("gh-issue-123".to_string())
        });
        assert!(result.is_ok());
        let record = result.unwrap();
        assert_eq!(record.remote_id, "gh-issue-123");
        assert_eq!(record.connector_id, "github");
        assert_eq!(record.sync_direction, SyncDirection::Outbound);
    }

    #[test]
    fn sync_issue_reuses_existing_remote_id() {
        let mut mgr = IssueSyncManager::new();
        let req = make_request("Bloat detected");
        let local_id = format!("postgresai:{}", req.title);
        mgr.register_sync(&local_id, "gh-existing-99", "github");

        let mut create_called = false;
        let result = mgr.sync_issue("postgresai", "github", &req, |_| {
            create_called = true;
            Ok("gh-new-999".to_string())
        });

        assert!(result.is_ok());
        assert!(
            !create_called,
            "create_fn must not be called for known issue"
        );
        assert_eq!(result.unwrap().remote_id, "gh-existing-99");
    }

    #[test]
    fn sync_issue_propagates_create_fn_error() {
        let mut mgr = IssueSyncManager::new();
        let req = make_request("Index bloat");
        let result = mgr.sync_issue("postgresai", "github", &req, |_| {
            Err(ConnectorError::AuthError("bad token".to_string()))
        });
        assert!(result.is_err());
        assert!(
            mgr.records().is_empty(),
            "no record should be stored on error"
        );
    }

    #[test]
    fn sync_issue_records_mapping_after_creation() {
        let mut mgr = IssueSyncManager::new();
        let req = make_request("Replication lag");
        mgr.sync_issue("postgresai", "jira", &req, |_| Ok("JIRA-42".to_string()))
            .unwrap();
        assert_eq!(mgr.records().len(), 1);
        let local_id = format!("postgresai:{}", req.title);
        assert_eq!(mgr.find_remote_id(&local_id, "jira"), Some("JIRA-42"));
    }
}
