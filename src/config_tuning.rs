//! Config tuning Analyzer — detects suboptimal `PostgreSQL` GUC settings.
//!
//! Operates at Observe level: reads `pg_settings` to produce structured
//! findings. No writes are performed.
//!
//! # Sub-findings
//!
//! | Sub-finding | Evidence Class | Source |
//! |---|---|---|
//! | `shared_buffers` too small | Heuristic | `pg_settings` vs system RAM |
//! | `effective_cache_size` too small | Heuristic | `pg_settings` vs system RAM |
//! | `work_mem` too low | Heuristic | `pg_settings` threshold |
//! | `maintenance_work_mem` too low | Heuristic | `pg_settings` threshold |
//! | `checkpoint_completion_target` non-optimal | Heuristic | `pg_settings` |
//! | `random_page_cost` too high for SSD | Advisory | `pg_settings` |
//! | `idle_in_transaction_session_timeout` disabled | Advisory | `pg_settings` |
//! | `statement_timeout` disabled | Advisory | `pg_settings` |
//! | Restart-required GUCs | Heuristic | `pg_settings` context |

// Phase 2/3 infrastructure — compiled but not yet wired into the main dispatch
// loop. Items are exercised via unit tests and will be connected in Phase 3.
#![allow(dead_code)]

use crate::governance::{EvidenceClass, Severity};

use std::fmt::Write as _;

// ---------------------------------------------------------------------------
// Config finding types
// ---------------------------------------------------------------------------

/// Category of configuration tuning finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigFindingKind {
    /// `shared_buffers` is significantly below 25% of system RAM.
    SharedBuffersTooSmall,
    /// `effective_cache_size` is significantly below 75% of system RAM.
    EffectiveCacheSizeTooSmall,
    /// `work_mem` is below the minimum recommended for analytical queries.
    WorkMemTooLow,
    /// `maintenance_work_mem` is below the minimum for maintenance operations.
    MaintenanceWorkMemTooLow,
    /// `checkpoint_completion_target` is set to the default 0.5 instead of 0.9.
    CheckpointCompletionTargetLow,
    /// `random_page_cost` is set to the spinning-disk default on likely-SSD storage.
    RandomPageCostTooHigh,
    /// `idle_in_transaction_session_timeout` is disabled (set to 0).
    IdleInTransactionTimeoutDisabled,
    /// `statement_timeout` is disabled (set to 0) — risky for production.
    StatementTimeoutDisabled,
    /// A GUC that would improve performance requires a server restart.
    RestartRequiredGuc,
}

impl ConfigFindingKind {
    /// Evidence class for this finding kind.
    #[allow(dead_code)]
    pub fn evidence_class(self) -> EvidenceClass {
        match self {
            Self::RandomPageCostTooHigh
            | Self::IdleInTransactionTimeoutDisabled
            | Self::StatementTimeoutDisabled => EvidenceClass::Advisory,
            _ => EvidenceClass::Heuristic,
        }
    }

    /// Human-readable label.
    pub fn label(self) -> &'static str {
        match self {
            Self::SharedBuffersTooSmall => "shared_buffers_too_small",
            Self::EffectiveCacheSizeTooSmall => "effective_cache_size_too_small",
            Self::WorkMemTooLow => "work_mem_too_low",
            Self::MaintenanceWorkMemTooLow => "maintenance_work_mem_too_low",
            Self::CheckpointCompletionTargetLow => "checkpoint_completion_target_low",
            Self::RandomPageCostTooHigh => "random_page_cost_too_high",
            Self::IdleInTransactionTimeoutDisabled => "idle_in_transaction_timeout_disabled",
            Self::StatementTimeoutDisabled => "statement_timeout_disabled",
            Self::RestartRequiredGuc => "restart_required_guc",
        }
    }
}

/// A single configuration tuning finding.
#[derive(Debug, Clone)]
pub struct ConfigFinding {
    /// What kind of finding.
    pub kind: ConfigFindingKind,
    /// GUC parameter name.
    pub parameter: String,
    /// Current value as reported by `PostgreSQL`.
    pub current_value: String,
    /// Human-readable description.
    pub description: String,
    /// Severity level.
    pub severity: Severity,
    /// Evidence class.
    #[allow(dead_code)]
    pub evidence_class: EvidenceClass,
    /// Suggested remediation (Observe mode: informational only).
    pub suggested_action: Option<String>,
    /// Recommended value for the GUC as a `PostgreSQL`-format string (e.g.
    /// `"0.9"`, `"4MB"`).  `None` when no single concrete value applies.
    pub recommended_value: Option<String>,
    /// Whether applying this change requires a full server restart.
    /// `true` when the GUC's context is `"postmaster"`.
    pub requires_restart: bool,
}

/// Complete config tuning report.
#[derive(Debug, Clone)]
pub struct ConfigTuningReport {
    /// All findings, sorted by severity (critical first).
    pub findings: Vec<ConfigFinding>,
}

impl ConfigTuningReport {
    /// Display the report to the terminal.
    pub fn display(&self) {
        if self.findings.is_empty() {
            eprintln!("Config tuning: no issues found.");
            return;
        }
        eprintln!(
            "Config tuning: {} issue{} found.\n",
            self.findings.len(),
            if self.findings.len() == 1 { "" } else { "s" }
        );
        for f in &self.findings {
            let icon = match f.severity {
                Severity::Critical => "!!",
                Severity::Warning => "! ",
                Severity::Info => "  ",
            };
            eprintln!(
                "{icon} [{}] {} = {}",
                f.kind.label(),
                f.parameter,
                f.current_value,
            );
            eprintln!("   {}", f.description);
            if let Some(ref action) = f.suggested_action {
                eprintln!("   suggestion: {action}");
            }
            eprintln!();
        }
    }

    /// Build a text summary for LLM consumption.
    #[allow(dead_code)]
    pub fn to_prompt(&self) -> String {
        if self.findings.is_empty() {
            return "No config tuning issues found.".to_owned();
        }
        let mut out = format!(
            "Config tuning report: {} finding(s)\n\n",
            self.findings.len()
        );
        for (i, f) in self.findings.iter().enumerate() {
            let _ = writeln!(
                out,
                "{}. [{}] {} = {}: {}",
                i + 1,
                f.kind.label(),
                f.parameter,
                f.current_value,
                f.description
            );
            if let Some(ref action) = f.suggested_action {
                let _ = writeln!(out, "   Suggested: {action}");
            }
            out.push('\n');
        }
        out
    }

    /// Collect all actionable proposals from this report.
    ///
    /// Returns one [`crate::governance::ActionProposal`] for each finding that
    /// has a concrete `recommended_value` **and** does not require a server
    /// restart.  Restart-required and purely advisory findings return `None`
    /// from [`ConfigFinding::to_proposal`] and are omitted.
    #[allow(dead_code)]
    pub fn to_proposals(&self) -> Vec<crate::governance::ActionProposal> {
        self.findings
            .iter()
            .filter_map(ConfigFinding::to_proposal)
            .collect()
    }
}

impl ConfigFinding {
    /// Convert this finding into an [`crate::governance::ActionProposal`].
    ///
    /// Returns `Some` when:
    /// - `recommended_value` is set (a concrete target value is known), **and**
    /// - `requires_restart` is `false` (the change is reload-safe via
    ///   `select pg_reload_conf()`).
    ///
    /// Returns `None` for restart-required GUCs and advisory findings without
    /// a single concrete recommended value.
    #[allow(dead_code)]
    pub fn to_proposal(&self) -> Option<crate::governance::ActionProposal> {
        // Only propose when we have a concrete value and no restart is needed.
        let recommended = self.recommended_value.as_ref()?;
        if self.requires_restart {
            return None;
        }

        let guc = &self.parameter;
        let proposed_action =
            format!("alter system set {guc} = '{recommended}'; select pg_reload_conf()");
        let expected_outcome = format!(
            "Set {guc} to {recommended} and reload PostgreSQL configuration \
             without a server restart"
        );
        let risk = format!(
            "alter system set writes to postgresql.auto.conf. \
             Verify the new value is appropriate for your workload before \
             applying. The change takes effect immediately after \
             pg_reload_conf() — no restart needed for {guc}."
        );

        Some(crate::governance::ActionProposal {
            feature: crate::governance::FeatureArea::ConfigTuning,
            severity: self.severity,
            evidence_class: self.evidence_class,
            finding: self.description.clone(),
            proposed_action,
            expected_outcome,
            risk,
            created_at: std::time::SystemTime::now(),
        })
    }
}

// ---------------------------------------------------------------------------
// SQL query
// ---------------------------------------------------------------------------

/// Fetch the GUC settings we care about from `pg_settings`.
const GUC_QUERY_SQL: &str = "\
    select \
        name, \
        setting, \
        unit, \
        context \
    from pg_settings \
    where name in (\
        'shared_buffers', \
        'work_mem', \
        'maintenance_work_mem', \
        'effective_cache_size', \
        'checkpoint_completion_target', \
        'random_page_cost', \
        'effective_io_concurrency', \
        'idle_in_transaction_session_timeout', \
        'lock_timeout', \
        'statement_timeout', \
        'max_connections', \
        'wal_buffers', \
        'min_wal_size', \
        'max_wal_size'\
    ) \
    order by name";

// ---------------------------------------------------------------------------
// Public analyzer
// ---------------------------------------------------------------------------

/// Collect config tuning findings from the database.
///
/// Runs diagnostic queries against `pg_settings`.
/// All operations are read-only (Observe mode).
pub async fn analyze(client: &tokio_postgres::Client) -> ConfigTuningReport {
    let mut findings = Vec::new();

    // Collect all GUC values first.
    let gucs = collect_gucs(client).await;

    // Attempt to detect system RAM from `pg_settings` hints.
    // We derive a RAM estimate from shared_buffers * 4 as a conservative
    // lower bound if no better source is available.
    let ram_bytes = estimate_system_ram(&gucs);

    // Evaluate each recommendation.
    check_shared_buffers(&gucs, ram_bytes, &mut findings);
    check_effective_cache_size(&gucs, ram_bytes, &mut findings);
    check_work_mem(&gucs, &mut findings);
    check_maintenance_work_mem(&gucs, &mut findings);
    check_checkpoint_completion_target(&gucs, &mut findings);
    check_random_page_cost(&gucs, &mut findings);
    check_idle_in_transaction_timeout(&gucs, &mut findings);
    check_statement_timeout(&gucs, &mut findings);
    check_restart_required_gucs(&gucs, ram_bytes, &mut findings);

    // Sort: Critical first, then Warning, then Info.
    findings.sort_by(|a, b| b.severity.cmp(&a.severity));

    ConfigTuningReport { findings }
}

// ---------------------------------------------------------------------------
// GUC collection
// ---------------------------------------------------------------------------

/// A single row from `pg_settings`.
#[derive(Debug, Clone)]
struct GucRow {
    name: String,
    /// Raw setting value (without unit suffix).
    setting: String,
    /// Unit string from `pg_settings.unit` (e.g. "8kB", "kB", "B", or empty).
    unit: String,
    /// Change context: "postmaster", "sighup", "superuser", etc.
    context: String,
}

impl GucRow {
    /// Resolve the setting to bytes.
    ///
    /// Uses the `unit` column from `pg_settings` to convert the raw numeric
    /// `setting` value. Returns `None` if the setting is not a memory value
    /// or cannot be parsed.
    fn setting_bytes(&self) -> Option<i64> {
        let raw: i64 = self.setting.parse().ok()?;
        // pg_settings reports memory in the unit's base, e.g.:
        //   shared_buffers: unit = "8kB", setting = "16384"
        //   → 16384 * 8192 bytes = 128 MiB
        //   work_mem: unit = "kB", setting = "4096"
        //   → 4096 * 1024 bytes = 4 MiB
        let multiplier: i64 = match self.unit.as_str() {
            "8kB" => 8 * 1024,
            "kB" => 1024,
            "MB" => 1024 * 1024,
            "GB" => 1024 * 1024 * 1024,
            "B" => 1,
            _ => return None,
        };
        Some(raw * multiplier)
    }

    /// Returns `true` if this GUC requires a full server restart to take effect.
    fn requires_restart(&self) -> bool {
        self.context == "postmaster"
    }
}

/// Collect all relevant GUC rows from the database.
async fn collect_gucs(client: &tokio_postgres::Client) -> Vec<GucRow> {
    let Ok(messages) = client.simple_query(GUC_QUERY_SQL).await else {
        return Vec::new();
    };
    let mut rows = Vec::new();
    for msg in messages {
        if let tokio_postgres::SimpleQueryMessage::Row(row) = msg {
            rows.push(GucRow {
                name: row.get(0).unwrap_or("").to_owned(),
                setting: row.get(1).unwrap_or("").to_owned(),
                unit: row.get(2).unwrap_or("").to_owned(),
                context: row.get(3).unwrap_or("").to_owned(),
            });
        }
    }
    rows
}

/// Look up a GUC by name.
fn find_guc<'a>(gucs: &'a [GucRow], name: &str) -> Option<&'a GucRow> {
    gucs.iter().find(|g| g.name == name)
}

// ---------------------------------------------------------------------------
// RAM estimation
// ---------------------------------------------------------------------------

/// Estimate total system RAM in bytes.
///
/// `PostgreSQL` does not directly expose total system RAM through `pg_settings`
/// in all versions. We use the following cascade:
///
/// 1. If `shared_buffers` is set, assume it is ~25% of RAM (rough heuristic
///    giving a 4× multiplier). This is a fallback only.
/// 2. We query `pg_settings` for `shared_buffers` raw bytes and multiply
///    by 4 as a very conservative lower-bound estimate.
///
/// This is intentionally conservative so we never over-report "too small".
fn estimate_system_ram(gucs: &[GucRow]) -> Option<i64> {
    let sb = find_guc(gucs, "shared_buffers")?;
    let sb_bytes = sb.setting_bytes()?;

    // shared_buffers is conventionally set to 25% of RAM.
    // Multiply by 4 to estimate total RAM.
    Some(sb_bytes * 4)
}

// ---------------------------------------------------------------------------
// Individual checks
// ---------------------------------------------------------------------------

/// Check `shared_buffers` against recommended 25% of system RAM.
fn check_shared_buffers(
    gucs: &[GucRow],
    ram_bytes: Option<i64>,
    findings: &mut Vec<ConfigFinding>,
) {
    let Some(guc) = find_guc(gucs, "shared_buffers") else {
        return;
    };
    let Some(current_bytes) = guc.setting_bytes() else {
        return;
    };
    let Some(ram) = ram_bytes else {
        return;
    };

    // Recommended: ~25% of system RAM.
    let recommended = ram / 4;

    // Only flag if current is less than 50% of recommended (i.e. < 12.5% RAM).
    if current_bytes >= recommended / 2 {
        return;
    }

    let current_mib = current_bytes / (1024 * 1024);
    let recommended_mib = recommended / (1024 * 1024);

    findings.push(ConfigFinding {
        kind: ConfigFindingKind::SharedBuffersTooSmall,
        parameter: "shared_buffers".to_owned(),
        current_value: format_memory_pg(current_bytes),
        description: format!(
            "shared_buffers is {current_mib} MiB, recommended ~{recommended_mib} MiB \
             (25% of estimated {ram_mib} MiB system RAM)",
            ram_mib = ram / (1024 * 1024),
        ),
        severity: if guc.requires_restart() {
            Severity::Info
        } else {
            Severity::Warning
        },
        evidence_class: EvidenceClass::Heuristic,
        suggested_action: Some(format!(
            "Set shared_buffers = '{recommended_mib}MB' in postgresql.conf \
             (requires restart)"
        )),
        recommended_value: Some(format!("{recommended_mib}MB")),
        requires_restart: guc.requires_restart(),
    });
}

/// Check `effective_cache_size` against recommended 75% of system RAM.
fn check_effective_cache_size(
    gucs: &[GucRow],
    ram_bytes: Option<i64>,
    findings: &mut Vec<ConfigFinding>,
) {
    let Some(guc) = find_guc(gucs, "effective_cache_size") else {
        return;
    };
    let Some(current_bytes) = guc.setting_bytes() else {
        return;
    };
    let Some(ram) = ram_bytes else {
        return;
    };

    // Recommended: ~75% of system RAM.
    let recommended = (ram / 4) * 3;

    // Only flag if current is less than half of recommended.
    if current_bytes >= recommended / 2 {
        return;
    }

    let current_mib = current_bytes / (1024 * 1024);
    let recommended_mib = recommended / (1024 * 1024);

    findings.push(ConfigFinding {
        kind: ConfigFindingKind::EffectiveCacheSizeTooSmall,
        parameter: "effective_cache_size".to_owned(),
        current_value: format_memory_pg(current_bytes),
        description: format!(
            "effective_cache_size is {current_mib} MiB, recommended ~{recommended_mib} MiB \
             (75% of estimated {ram_mib} MiB system RAM); \
             this affects planner index-vs-seqscan decisions",
            ram_mib = ram / (1024 * 1024),
        ),
        // effective_cache_size is reload-safe (sighup context).
        severity: Severity::Warning,
        evidence_class: EvidenceClass::Heuristic,
        suggested_action: Some(format!(
            "Set effective_cache_size = '{recommended_mib}MB' in postgresql.conf \
             (reload-safe: SELECT pg_reload_conf())"
        )),
        recommended_value: Some(format!("{recommended_mib}MB")),
        requires_restart: false,
    });
}

/// Minimum recommended `work_mem` (4 MiB).
const MIN_WORK_MEM: i64 = 4 * 1024 * 1024;

/// Minimum recommended `maintenance_work_mem` (64 MiB).
const MIN_MAINTENANCE_WORK_MEM: i64 = 64 * 1024 * 1024;

/// Check `work_mem` — must be >= 4 MiB for analytical queries.
fn check_work_mem(gucs: &[GucRow], findings: &mut Vec<ConfigFinding>) {
    let Some(guc) = find_guc(gucs, "work_mem") else {
        return;
    };
    let Some(current_bytes) = guc.setting_bytes() else {
        return;
    };

    if current_bytes >= MIN_WORK_MEM {
        return;
    }

    let current_kib = current_bytes / 1024;

    findings.push(ConfigFinding {
        kind: ConfigFindingKind::WorkMemTooLow,
        parameter: "work_mem".to_owned(),
        current_value: format_memory_pg(current_bytes),
        description: format!(
            "work_mem is {current_kib} KiB, below the recommended minimum of 4096 KiB (4 MiB); \
             low work_mem forces hash and sort operations to spill to disk"
        ),
        severity: Severity::Warning,
        evidence_class: EvidenceClass::Heuristic,
        suggested_action: Some(
            "Set work_mem = '4MB' in postgresql.conf (reload-safe: SELECT pg_reload_conf())"
                .to_owned(),
        ),
        recommended_value: Some("4MB".to_owned()),
        requires_restart: false,
    });
}

/// Check `maintenance_work_mem` — must be >= 64 MiB.
fn check_maintenance_work_mem(gucs: &[GucRow], findings: &mut Vec<ConfigFinding>) {
    let Some(guc) = find_guc(gucs, "maintenance_work_mem") else {
        return;
    };
    let Some(current_bytes) = guc.setting_bytes() else {
        return;
    };

    if current_bytes >= MIN_MAINTENANCE_WORK_MEM {
        return;
    }

    let current_mib = current_bytes / (1024 * 1024);

    findings.push(ConfigFinding {
        kind: ConfigFindingKind::MaintenanceWorkMemTooLow,
        parameter: "maintenance_work_mem".to_owned(),
        current_value: format_memory_pg(current_bytes),
        description: format!(
            "maintenance_work_mem is {current_mib} MiB, below the recommended 64 MiB; \
             low maintenance_work_mem slows VACUUM, CREATE INDEX, and pg_restore"
        ),
        severity: Severity::Warning,
        evidence_class: EvidenceClass::Heuristic,
        suggested_action: Some(
            "Set maintenance_work_mem = '64MB' in postgresql.conf \
             (reload-safe: SELECT pg_reload_conf())"
                .to_owned(),
        ),
        recommended_value: Some("64MB".to_owned()),
        requires_restart: false,
    });
}

/// Check `checkpoint_completion_target` — should be 0.9, not the default 0.5.
fn check_checkpoint_completion_target(gucs: &[GucRow], findings: &mut Vec<ConfigFinding>) {
    let Some(guc) = find_guc(gucs, "checkpoint_completion_target") else {
        return;
    };
    let Ok(current) = guc.setting.parse::<f64>() else {
        return;
    };

    // Default is 0.5 (pre-PG 17) or 0.9 (PG 17+). We recommend >= 0.9.
    if current >= 0.9 {
        return;
    }

    findings.push(ConfigFinding {
        kind: ConfigFindingKind::CheckpointCompletionTargetLow,
        parameter: "checkpoint_completion_target".to_owned(),
        current_value: guc.setting.clone(),
        description: format!(
            "checkpoint_completion_target is {current:.1}, \
             the recommended value is 0.9; \
             a low value causes I/O spikes at checkpoint completion"
        ),
        severity: Severity::Warning,
        evidence_class: EvidenceClass::Heuristic,
        suggested_action: Some(
            "Set checkpoint_completion_target = 0.9 in postgresql.conf \
             (reload-safe: SELECT pg_reload_conf())"
                .to_owned(),
        ),
        recommended_value: Some("0.9".to_owned()),
        requires_restart: false,
    });
}

/// Check `random_page_cost` — should be 1.1 for SSD storage.
///
/// The default of 4.0 assumes spinning-disk I/O patterns. On SSDs, random
/// reads are nearly as fast as sequential reads so 1.1 is appropriate.
/// This is Advisory because we cannot detect storage type from `pg_settings`.
fn check_random_page_cost(gucs: &[GucRow], findings: &mut Vec<ConfigFinding>) {
    let Some(guc) = find_guc(gucs, "random_page_cost") else {
        return;
    };
    let Ok(current) = guc.setting.parse::<f64>() else {
        return;
    };

    // Only flag the spinning-disk default or higher.
    if current < 3.5 {
        return;
    }

    findings.push(ConfigFinding {
        kind: ConfigFindingKind::RandomPageCostTooHigh,
        parameter: "random_page_cost".to_owned(),
        current_value: guc.setting.clone(),
        description: format!(
            "random_page_cost is {current:.1} (spinning-disk default); \
             if this server uses SSD storage, set it to 1.1 to improve \
             index-vs-seqscan decisions"
        ),
        severity: Severity::Critical,
        evidence_class: EvidenceClass::Advisory,
        suggested_action: Some(
            "Set random_page_cost = 1.1 in postgresql.conf if using SSD storage \
             (reload-safe: SELECT pg_reload_conf())"
                .to_owned(),
        ),
        // Advisory — cannot confirm storage type; no concrete proposal.
        recommended_value: None,
        requires_restart: false,
    });
}

/// Check `idle_in_transaction_session_timeout` — 0 (disabled) is dangerous.
fn check_idle_in_transaction_timeout(gucs: &[GucRow], findings: &mut Vec<ConfigFinding>) {
    let Some(guc) = find_guc(gucs, "idle_in_transaction_session_timeout") else {
        return;
    };
    let Ok(current_ms) = guc.setting.parse::<i64>() else {
        return;
    };

    if current_ms > 0 {
        return;
    }

    findings.push(ConfigFinding {
        kind: ConfigFindingKind::IdleInTransactionTimeoutDisabled,
        parameter: "idle_in_transaction_session_timeout".to_owned(),
        current_value: guc.setting.clone(),
        description: "idle_in_transaction_session_timeout is disabled (0); \
             sessions stuck in idle-in-transaction hold locks indefinitely, \
             causing table bloat and connection pile-ups"
            .to_owned(),
        severity: Severity::Critical,
        evidence_class: EvidenceClass::Advisory,
        suggested_action: Some(
            "Set idle_in_transaction_session_timeout = '5min' in postgresql.conf \
             (reload-safe: SELECT pg_reload_conf())"
                .to_owned(),
        ),
        recommended_value: Some("5min".to_owned()),
        requires_restart: false,
    });
}

/// Check `statement_timeout` — 0 (disabled) may be risky in production.
fn check_statement_timeout(gucs: &[GucRow], findings: &mut Vec<ConfigFinding>) {
    let Some(guc) = find_guc(gucs, "statement_timeout") else {
        return;
    };
    let Ok(current_ms) = guc.setting.parse::<i64>() else {
        return;
    };

    if current_ms > 0 {
        return;
    }

    findings.push(ConfigFinding {
        kind: ConfigFindingKind::StatementTimeoutDisabled,
        parameter: "statement_timeout".to_owned(),
        current_value: guc.setting.clone(),
        description: "statement_timeout is disabled (0); \
             runaway queries can hold resources indefinitely on production systems"
            .to_owned(),
        severity: Severity::Warning,
        evidence_class: EvidenceClass::Advisory,
        suggested_action: Some(
            "Consider setting statement_timeout to a value appropriate for your \
             workload (e.g. '30s' or '5min') in postgresql.conf \
             (reload-safe: SELECT pg_reload_conf())"
                .to_owned(),
        ),
        // Advisory — workload-specific; no single concrete value to propose.
        recommended_value: None,
        requires_restart: false,
    });
}

/// Report GUCs that would benefit from tuning but require a server restart.
///
/// These are reported as Info since they cannot be reloaded live, and we
/// want to flag them for operators to act on at the next maintenance window.
fn check_restart_required_gucs(
    gucs: &[GucRow],
    ram_bytes: Option<i64>,
    findings: &mut Vec<ConfigFinding>,
) {
    // shared_buffers requires restart — flag if far from ideal and not already
    // flagged by check_shared_buffers (which also fires for this condition).
    // We only add an Info-level restart notice for `max_connections` here
    // since there is no separate check for it.
    let Some(mc_guc) = find_guc(gucs, "max_connections") else {
        return;
    };
    let Ok(max_conn) = mc_guc.setting.parse::<i64>() else {
        return;
    };

    // Flag very high max_connections with no pooler evidence.
    // Over 200 is often a sign that a connection pooler is not in use.
    if max_conn > 200 {
        findings.push(ConfigFinding {
            kind: ConfigFindingKind::RestartRequiredGuc,
            parameter: "max_connections".to_owned(),
            current_value: mc_guc.setting.clone(),
            description: format!(
                "max_connections is {max_conn}; high values increase memory overhead \
                 per connection — consider using a connection pooler (e.g. PgBouncer) \
                 and lowering max_connections"
            ),
            severity: Severity::Info,
            evidence_class: EvidenceClass::Heuristic,
            suggested_action: Some(
                "Deploy a connection pooler and lower max_connections \
                 (requires restart)"
                    .to_owned(),
            ),
            // No concrete recommended value — depends on the workload.
            recommended_value: None,
            requires_restart: true,
        });
    }

    // wal_buffers: if set to the old tiny default of 64kB.
    if let Some(wb_guc) = find_guc(gucs, "wal_buffers") {
        if let Some(wb_bytes) = wb_guc.setting_bytes() {
            // Recommended: at least 16 MiB (or auto-tuned from shared_buffers).
            // The old default was 64kB; PG 9.1+ auto-tunes it to 3% of
            // shared_buffers. Flag only the literal old tiny default.
            let very_small = 64 * 1024; // 64 KiB
            if wb_bytes <= very_small {
                let wb_kib = wb_bytes / 1024;
                let recommended_mib = ram_bytes
                    .map_or(16, |r| r / 64 / (1024 * 1024)) // ~1.5% of RAM
                    .max(16);
                findings.push(ConfigFinding {
                    kind: ConfigFindingKind::RestartRequiredGuc,
                    parameter: "wal_buffers".to_owned(),
                    current_value: format_memory_pg(wb_bytes),
                    description: format!(
                        "wal_buffers is {wb_kib} KiB, well below the recommended \
                         {recommended_mib} MiB; small wal_buffers increase WAL write latency"
                    ),
                    severity: Severity::Info,
                    evidence_class: EvidenceClass::Heuristic,
                    suggested_action: Some(format!(
                        "Set wal_buffers = '{recommended_mib}MB' in postgresql.conf \
                         (requires restart)"
                    )),
                    recommended_value: Some(format!("{recommended_mib}MB")),
                    requires_restart: true,
                });
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Display helpers
// ---------------------------------------------------------------------------

/// Format bytes as a `PostgreSQL` config value string (e.g. "128MB", "4GB").
///
/// Uses PG-style units (MB, GB) — not binary (MiB, GiB) — because these
/// values appear as recommended postgresql.conf settings.
#[allow(clippy::cast_precision_loss)]
pub fn format_memory_pg(bytes: i64) -> String {
    const MB: i64 = 1024 * 1024;
    const GB: i64 = 1024 * MB;

    if bytes >= GB && bytes % GB == 0 {
        format!("{}GB", bytes / GB)
    } else if bytes >= MB {
        // Round to nearest MB for readability.
        format!("{}MB", (bytes + MB / 2) / MB)
    } else {
        format!("{}kB", (bytes + 511) / 1024)
    }
}

/// Format bytes as a human-readable display string using binary units (GiB, MiB, KiB).
///
/// Used for descriptions shown in the terminal — binary units per project convention.
#[allow(clippy::cast_precision_loss)]
pub fn format_bytes_display(bytes: i64) -> String {
    const KIB: i64 = 1024;
    const MIB: i64 = 1024 * KIB;
    const GIB: i64 = 1024 * MIB;

    if bytes >= GIB {
        format!("{:.1} GiB", bytes as f64 / GIB as f64)
    } else if bytes >= MIB {
        format!("{:.1} MiB", bytes as f64 / MIB as f64)
    } else if bytes >= KIB {
        format!("{:.1} KiB", bytes as f64 / KIB as f64)
    } else {
        format!("{bytes} B")
    }
}

// ---------------------------------------------------------------------------
// Public memory parsing helper (used in tests and by callers)
// ---------------------------------------------------------------------------

/// Parse a `PostgreSQL` memory value string (e.g. "128MB", "1GB", "8kB", "512")
/// into bytes.
///
/// Recognises the suffixes accepted by `PostgreSQL`:
/// - `kB` / `KB` — kibibytes (1 024 bytes)
/// - `MB` — mebibytes (1 048 576 bytes)
/// - `GB` — gibibytes (1 073 741 824 bytes)
/// - `TB` — tebibytes
/// - `B` / no suffix — bytes
///
/// Returns `None` if the string cannot be parsed.
pub fn parse_pg_memory(s: &str) -> Option<i64> {
    let s = s.trim();

    // Try longest suffix first to avoid "GB" matching before "B".
    let (num_str, mult) = if let Some(n) = s.strip_suffix("TB") {
        (n, 1024_i64 * 1024 * 1024 * 1024)
    } else if let Some(n) = s.strip_suffix("GB") {
        (n, 1024_i64 * 1024 * 1024)
    } else if let Some(n) = s.strip_suffix("MB") {
        (n, 1024_i64 * 1024)
    } else if let Some(n) = s.strip_suffix("kB") {
        (n, 1024_i64)
    } else if let Some(n) = s.strip_suffix("KB") {
        (n, 1024_i64)
    } else if let Some(n) = s.strip_suffix('B') {
        (n, 1_i64)
    } else {
        // No suffix — treat as bytes.
        (s, 1_i64)
    };

    let num: i64 = num_str.trim().parse().ok()?;
    Some(num * mult)
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // parse_pg_memory
    // -----------------------------------------------------------------------

    #[test]
    fn parse_pg_memory_bytes() {
        assert_eq!(parse_pg_memory("1024"), Some(1024));
        assert_eq!(parse_pg_memory("512B"), Some(512));
    }

    #[test]
    fn parse_pg_memory_kibibytes() {
        assert_eq!(parse_pg_memory("8kB"), Some(8 * 1024));
        assert_eq!(parse_pg_memory("64KB"), Some(64 * 1024));
    }

    #[test]
    fn parse_pg_memory_mebibytes() {
        assert_eq!(parse_pg_memory("128MB"), Some(128 * 1024 * 1024));
        assert_eq!(parse_pg_memory("1MB"), Some(1024 * 1024));
    }

    #[test]
    fn parse_pg_memory_gibibytes() {
        assert_eq!(parse_pg_memory("1GB"), Some(1024_i64 * 1024 * 1024));
        assert_eq!(parse_pg_memory("4GB"), Some(4 * 1024_i64 * 1024 * 1024));
    }

    #[test]
    fn parse_pg_memory_tebibytes() {
        assert_eq!(parse_pg_memory("1TB"), Some(1024_i64 * 1024 * 1024 * 1024));
    }

    #[test]
    fn parse_pg_memory_whitespace() {
        assert_eq!(parse_pg_memory("  128MB  "), Some(128 * 1024 * 1024));
    }

    #[test]
    fn parse_pg_memory_invalid() {
        assert_eq!(parse_pg_memory(""), None);
        assert_eq!(parse_pg_memory("notanumber"), None);
        assert_eq!(parse_pg_memory("notanumberMB"), None);
    }

    // -----------------------------------------------------------------------
    // format_memory_pg
    // -----------------------------------------------------------------------

    #[test]
    fn format_memory_pg_gigabytes_exact() {
        assert_eq!(format_memory_pg(1024 * 1024 * 1024), "1GB");
        assert_eq!(format_memory_pg(4 * 1024 * 1024 * 1024), "4GB");
    }

    #[test]
    fn format_memory_pg_megabytes() {
        assert_eq!(format_memory_pg(128 * 1024 * 1024), "128MB");
        assert_eq!(format_memory_pg(64 * 1024 * 1024), "64MB");
    }

    #[test]
    fn format_memory_pg_kilobytes() {
        assert_eq!(format_memory_pg(8 * 1024), "8kB");
    }

    // -----------------------------------------------------------------------
    // format_bytes_display
    // -----------------------------------------------------------------------

    #[test]
    fn format_bytes_display_gib() {
        let bytes = 2 * 1024 * 1024 * 1024_i64;
        assert_eq!(format_bytes_display(bytes), "2.0 GiB");
    }

    #[test]
    fn format_bytes_display_mib() {
        let bytes = 256 * 1024 * 1024_i64;
        assert_eq!(format_bytes_display(bytes), "256.0 MiB");
    }

    #[test]
    fn format_bytes_display_kib() {
        assert_eq!(format_bytes_display(4096), "4.0 KiB");
    }

    #[test]
    fn format_bytes_display_bytes() {
        assert_eq!(format_bytes_display(512), "512 B");
    }

    // -----------------------------------------------------------------------
    // GucRow::setting_bytes
    // -----------------------------------------------------------------------

    #[test]
    fn guc_row_setting_bytes_8kb_unit() {
        // shared_buffers: unit "8kB", setting "16384" → 128 MiB
        let row = GucRow {
            name: "shared_buffers".to_owned(),
            setting: "16384".to_owned(),
            unit: "8kB".to_owned(),
            context: "postmaster".to_owned(),
        };
        assert_eq!(row.setting_bytes(), Some(16384 * 8 * 1024));
    }

    #[test]
    fn guc_row_setting_bytes_kb_unit() {
        // work_mem: unit "kB", setting "4096" → 4 MiB
        let row = GucRow {
            name: "work_mem".to_owned(),
            setting: "4096".to_owned(),
            unit: "kB".to_owned(),
            context: "user".to_owned(),
        };
        assert_eq!(row.setting_bytes(), Some(4 * 1024 * 1024));
    }

    #[test]
    fn guc_row_setting_bytes_non_memory() {
        // checkpoint_completion_target has no unit — returns None.
        let row = GucRow {
            name: "checkpoint_completion_target".to_owned(),
            setting: "0.5".to_owned(),
            unit: String::new(),
            context: "sighup".to_owned(),
        };
        assert_eq!(row.setting_bytes(), None);
    }

    #[test]
    fn guc_row_requires_restart() {
        let restart = GucRow {
            name: "shared_buffers".to_owned(),
            setting: "131072".to_owned(),
            unit: "8kB".to_owned(),
            context: "postmaster".to_owned(),
        };
        assert!(restart.requires_restart());

        let reload = GucRow {
            name: "work_mem".to_owned(),
            setting: "4096".to_owned(),
            unit: "kB".to_owned(),
            context: "user".to_owned(),
        };
        assert!(!reload.requires_restart());
    }

    // -----------------------------------------------------------------------
    // ConfigFindingKind
    // -----------------------------------------------------------------------

    #[test]
    fn finding_kind_labels() {
        assert_eq!(
            ConfigFindingKind::SharedBuffersTooSmall.label(),
            "shared_buffers_too_small"
        );
        assert_eq!(ConfigFindingKind::WorkMemTooLow.label(), "work_mem_too_low");
        assert_eq!(
            ConfigFindingKind::RandomPageCostTooHigh.label(),
            "random_page_cost_too_high"
        );
        assert_eq!(
            ConfigFindingKind::IdleInTransactionTimeoutDisabled.label(),
            "idle_in_transaction_timeout_disabled"
        );
        assert_eq!(
            ConfigFindingKind::StatementTimeoutDisabled.label(),
            "statement_timeout_disabled"
        );
        assert_eq!(
            ConfigFindingKind::RestartRequiredGuc.label(),
            "restart_required_guc"
        );
    }

    #[test]
    fn finding_kind_evidence_classes() {
        assert_eq!(
            ConfigFindingKind::SharedBuffersTooSmall.evidence_class(),
            EvidenceClass::Heuristic
        );
        assert_eq!(
            ConfigFindingKind::RandomPageCostTooHigh.evidence_class(),
            EvidenceClass::Advisory
        );
        assert_eq!(
            ConfigFindingKind::IdleInTransactionTimeoutDisabled.evidence_class(),
            EvidenceClass::Advisory
        );
        assert_eq!(
            ConfigFindingKind::StatementTimeoutDisabled.evidence_class(),
            EvidenceClass::Advisory
        );
        assert_eq!(
            ConfigFindingKind::WorkMemTooLow.evidence_class(),
            EvidenceClass::Heuristic
        );
    }

    // -----------------------------------------------------------------------
    // Individual check logic
    // -----------------------------------------------------------------------

    #[test]
    fn check_work_mem_fires_below_4mib() {
        let gucs = vec![GucRow {
            name: "work_mem".to_owned(),
            setting: "1024".to_owned(), // 1 MiB
            unit: "kB".to_owned(),
            context: "user".to_owned(),
        }];
        let mut findings = Vec::new();
        check_work_mem(&gucs, &mut findings);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, ConfigFindingKind::WorkMemTooLow);
        assert_eq!(findings[0].severity, Severity::Warning);
    }

    #[test]
    fn check_work_mem_no_finding_at_4mib() {
        let gucs = vec![GucRow {
            name: "work_mem".to_owned(),
            setting: "4096".to_owned(), // exactly 4 MiB
            unit: "kB".to_owned(),
            context: "user".to_owned(),
        }];
        let mut findings = Vec::new();
        check_work_mem(&gucs, &mut findings);
        assert!(findings.is_empty());
    }

    #[test]
    fn check_maintenance_work_mem_fires_below_64mib() {
        let gucs = vec![GucRow {
            name: "maintenance_work_mem".to_owned(),
            setting: "16384".to_owned(), // 16 MiB
            unit: "kB".to_owned(),
            context: "user".to_owned(),
        }];
        let mut findings = Vec::new();
        check_maintenance_work_mem(&gucs, &mut findings);
        assert_eq!(findings.len(), 1);
        assert_eq!(
            findings[0].kind,
            ConfigFindingKind::MaintenanceWorkMemTooLow
        );
    }

    #[test]
    fn check_maintenance_work_mem_no_finding_at_64mib() {
        let gucs = vec![GucRow {
            name: "maintenance_work_mem".to_owned(),
            setting: "65536".to_owned(), // 64 MiB
            unit: "kB".to_owned(),
            context: "user".to_owned(),
        }];
        let mut findings = Vec::new();
        check_maintenance_work_mem(&gucs, &mut findings);
        assert!(findings.is_empty());
    }

    #[test]
    fn check_checkpoint_target_fires_at_0_5() {
        let gucs = vec![GucRow {
            name: "checkpoint_completion_target".to_owned(),
            setting: "0.5".to_owned(),
            unit: String::new(),
            context: "sighup".to_owned(),
        }];
        let mut findings = Vec::new();
        check_checkpoint_completion_target(&gucs, &mut findings);
        assert_eq!(findings.len(), 1);
        assert_eq!(
            findings[0].kind,
            ConfigFindingKind::CheckpointCompletionTargetLow
        );
        assert_eq!(findings[0].severity, Severity::Warning);
    }

    #[test]
    fn check_checkpoint_target_no_finding_at_0_9() {
        let gucs = vec![GucRow {
            name: "checkpoint_completion_target".to_owned(),
            setting: "0.9".to_owned(),
            unit: String::new(),
            context: "sighup".to_owned(),
        }];
        let mut findings = Vec::new();
        check_checkpoint_completion_target(&gucs, &mut findings);
        assert!(findings.is_empty());
    }

    #[test]
    fn check_random_page_cost_fires_at_4() {
        let gucs = vec![GucRow {
            name: "random_page_cost".to_owned(),
            setting: "4".to_owned(),
            unit: String::new(),
            context: "user".to_owned(),
        }];
        let mut findings = Vec::new();
        check_random_page_cost(&gucs, &mut findings);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, ConfigFindingKind::RandomPageCostTooHigh);
        assert_eq!(findings[0].severity, Severity::Critical);
    }

    #[test]
    fn check_random_page_cost_no_finding_at_1_1() {
        let gucs = vec![GucRow {
            name: "random_page_cost".to_owned(),
            setting: "1.1".to_owned(),
            unit: String::new(),
            context: "user".to_owned(),
        }];
        let mut findings = Vec::new();
        check_random_page_cost(&gucs, &mut findings);
        assert!(findings.is_empty());
    }

    #[test]
    fn check_idle_in_transaction_timeout_fires_at_zero() {
        let gucs = vec![GucRow {
            name: "idle_in_transaction_session_timeout".to_owned(),
            setting: "0".to_owned(),
            unit: "ms".to_owned(),
            context: "user".to_owned(),
        }];
        let mut findings = Vec::new();
        check_idle_in_transaction_timeout(&gucs, &mut findings);
        assert_eq!(findings.len(), 1);
        assert_eq!(
            findings[0].kind,
            ConfigFindingKind::IdleInTransactionTimeoutDisabled
        );
        assert_eq!(findings[0].severity, Severity::Critical);
    }

    #[test]
    fn check_idle_in_transaction_timeout_no_finding_when_set() {
        let gucs = vec![GucRow {
            name: "idle_in_transaction_session_timeout".to_owned(),
            setting: "300000".to_owned(), // 5 minutes in ms
            unit: "ms".to_owned(),
            context: "user".to_owned(),
        }];
        let mut findings = Vec::new();
        check_idle_in_transaction_timeout(&gucs, &mut findings);
        assert!(findings.is_empty());
    }

    #[test]
    fn check_statement_timeout_fires_at_zero() {
        let gucs = vec![GucRow {
            name: "statement_timeout".to_owned(),
            setting: "0".to_owned(),
            unit: "ms".to_owned(),
            context: "user".to_owned(),
        }];
        let mut findings = Vec::new();
        check_statement_timeout(&gucs, &mut findings);
        assert_eq!(findings.len(), 1);
        assert_eq!(
            findings[0].kind,
            ConfigFindingKind::StatementTimeoutDisabled
        );
        assert_eq!(findings[0].severity, Severity::Warning);
    }

    #[test]
    fn check_statement_timeout_no_finding_when_set() {
        let gucs = vec![GucRow {
            name: "statement_timeout".to_owned(),
            setting: "30000".to_owned(), // 30 seconds
            unit: "ms".to_owned(),
            context: "user".to_owned(),
        }];
        let mut findings = Vec::new();
        check_statement_timeout(&gucs, &mut findings);
        assert!(findings.is_empty());
    }

    // -----------------------------------------------------------------------
    // Report
    // -----------------------------------------------------------------------

    #[test]
    fn empty_report_to_prompt() {
        let report = ConfigTuningReport {
            findings: Vec::new(),
        };
        assert!(report.to_prompt().contains("No config tuning issues found"));
    }

    #[test]
    fn report_to_prompt_with_findings() {
        let report = ConfigTuningReport {
            findings: vec![ConfigFinding {
                kind: ConfigFindingKind::WorkMemTooLow,
                parameter: "work_mem".to_owned(),
                current_value: "1MB".to_owned(),
                description: "work_mem is 1024 KiB, below 4096 KiB".to_owned(),
                severity: Severity::Warning,
                evidence_class: EvidenceClass::Heuristic,
                suggested_action: Some("Set work_mem = '4MB'".to_owned()),
                recommended_value: Some("4MB".to_owned()),
                requires_restart: false,
            }],
        };
        let prompt = report.to_prompt();
        assert!(prompt.contains("1 finding"));
        assert!(prompt.contains("[work_mem_too_low]"));
        assert!(prompt.contains("work_mem = 1MB"));
        assert!(prompt.contains("Set work_mem"));
    }

    #[test]
    fn report_sorts_critical_first() {
        let mut report = ConfigTuningReport {
            findings: vec![
                ConfigFinding {
                    kind: ConfigFindingKind::StatementTimeoutDisabled,
                    parameter: "statement_timeout".to_owned(),
                    current_value: "0".to_owned(),
                    description: "disabled".to_owned(),
                    severity: Severity::Warning,
                    evidence_class: EvidenceClass::Advisory,
                    suggested_action: None,
                    recommended_value: None,
                    requires_restart: false,
                },
                ConfigFinding {
                    kind: ConfigFindingKind::IdleInTransactionTimeoutDisabled,
                    parameter: "idle_in_transaction_session_timeout".to_owned(),
                    current_value: "0".to_owned(),
                    description: "disabled".to_owned(),
                    severity: Severity::Critical,
                    evidence_class: EvidenceClass::Advisory,
                    suggested_action: None,
                    recommended_value: None,
                    requires_restart: false,
                },
                ConfigFinding {
                    kind: ConfigFindingKind::RestartRequiredGuc,
                    parameter: "max_connections".to_owned(),
                    current_value: "500".to_owned(),
                    description: "high".to_owned(),
                    severity: Severity::Info,
                    evidence_class: EvidenceClass::Heuristic,
                    suggested_action: None,
                    recommended_value: None,
                    requires_restart: true,
                },
            ],
        };
        report.findings.sort_by(|a, b| b.severity.cmp(&a.severity));
        assert_eq!(report.findings[0].severity, Severity::Critical);
        assert_eq!(report.findings[1].severity, Severity::Warning);
        assert_eq!(report.findings[2].severity, Severity::Info);
    }

    // -----------------------------------------------------------------------
    // SQL constants
    // -----------------------------------------------------------------------

    #[test]
    fn guc_query_sql_contains_key_parameters() {
        assert!(GUC_QUERY_SQL.contains("shared_buffers"));
        assert!(GUC_QUERY_SQL.contains("work_mem"));
        assert!(GUC_QUERY_SQL.contains("random_page_cost"));
        assert!(GUC_QUERY_SQL.contains("idle_in_transaction_session_timeout"));
        assert!(GUC_QUERY_SQL.contains("statement_timeout"));
        assert!(GUC_QUERY_SQL.contains("checkpoint_completion_target"));
        assert!(GUC_QUERY_SQL.contains("pg_settings"));
    }

    #[test]
    fn guc_query_sql_selects_context() {
        assert!(GUC_QUERY_SQL.contains("context"));
    }

    // -----------------------------------------------------------------------
    // to_proposal / to_proposals tests
    // -----------------------------------------------------------------------

    /// Helper: build a minimal `ConfigFinding` for testing.
    fn make_config_finding(
        kind: ConfigFindingKind,
        parameter: &str,
        recommended_value: Option<&str>,
        requires_restart: bool,
        severity: Severity,
        evidence_class: EvidenceClass,
    ) -> ConfigFinding {
        ConfigFinding {
            kind,
            parameter: parameter.to_owned(),
            current_value: "old_value".to_owned(),
            description: format!("test finding for {parameter}"),
            severity,
            evidence_class,
            suggested_action: None,
            recommended_value: recommended_value.map(ToOwned::to_owned),
            requires_restart,
        }
    }

    #[test]
    fn reload_safe_finding_with_recommended_value_produces_proposal() {
        let f = make_config_finding(
            ConfigFindingKind::WorkMemTooLow,
            "work_mem",
            Some("4MB"),
            false,
            Severity::Warning,
            EvidenceClass::Heuristic,
        );
        let proposal = f
            .to_proposal()
            .expect("reload-safe + recommended_value should yield a proposal");
        assert_eq!(
            proposal.feature,
            crate::governance::FeatureArea::ConfigTuning
        );
        assert_eq!(proposal.severity, Severity::Warning);
        // SQL must use lowercase keywords and reference the GUC.
        assert!(
            proposal.proposed_action.contains("alter system set"),
            "proposed_action should contain 'alter system set': {}",
            proposal.proposed_action,
        );
        assert!(
            proposal.proposed_action.contains("work_mem"),
            "proposed_action should contain GUC name: {}",
            proposal.proposed_action,
        );
        assert!(
            proposal.proposed_action.contains("4MB"),
            "proposed_action should contain recommended value: {}",
            proposal.proposed_action,
        );
        assert!(
            proposal.proposed_action.contains("pg_reload_conf"),
            "proposed_action should call pg_reload_conf: {}",
            proposal.proposed_action,
        );
    }

    #[test]
    fn restart_required_finding_produces_no_proposal() {
        let f = make_config_finding(
            ConfigFindingKind::RestartRequiredGuc,
            "wal_buffers",
            Some("16MB"),
            true, // requires restart
            Severity::Info,
            EvidenceClass::Heuristic,
        );
        assert!(
            f.to_proposal().is_none(),
            "restart-required GUC should not produce a proposal"
        );
    }

    #[test]
    fn finding_without_recommended_value_produces_no_proposal() {
        // random_page_cost is Advisory with no concrete recommended_value.
        let f = make_config_finding(
            ConfigFindingKind::RandomPageCostTooHigh,
            "random_page_cost",
            None, // no concrete recommended value
            false,
            Severity::Critical,
            EvidenceClass::Advisory,
        );
        assert!(
            f.to_proposal().is_none(),
            "no recommended_value should yield no proposal"
        );
    }

    #[test]
    fn idle_in_transaction_timeout_produces_proposal() {
        let f = make_config_finding(
            ConfigFindingKind::IdleInTransactionTimeoutDisabled,
            "idle_in_transaction_session_timeout",
            Some("5min"),
            false,
            Severity::Critical,
            EvidenceClass::Advisory,
        );
        let proposal = f.to_proposal().expect(
            "idle_in_transaction_session_timeout with recommended value should yield a proposal",
        );
        assert!(
            proposal
                .proposed_action
                .contains("idle_in_transaction_session_timeout"),
            "proposed_action should reference GUC: {}",
            proposal.proposed_action,
        );
        assert!(
            proposal.proposed_action.contains("5min"),
            "proposed_action should include recommended value: {}",
            proposal.proposed_action,
        );
        assert_eq!(proposal.severity, Severity::Critical);
    }

    #[test]
    fn checkpoint_completion_target_produces_proposal() {
        let f = make_config_finding(
            ConfigFindingKind::CheckpointCompletionTargetLow,
            "checkpoint_completion_target",
            Some("0.9"),
            false,
            Severity::Warning,
            EvidenceClass::Heuristic,
        );
        let proposal = f
            .to_proposal()
            .expect("checkpoint_completion_target should yield a proposal");
        assert!(
            proposal
                .proposed_action
                .contains("checkpoint_completion_target"),
            "proposed_action should reference GUC: {}",
            proposal.proposed_action,
        );
        assert!(
            proposal.proposed_action.contains("0.9"),
            "proposed_action should include recommended value: {}",
            proposal.proposed_action,
        );
    }

    #[test]
    fn to_proposals_filters_out_restart_and_no_value() {
        let report = ConfigTuningReport {
            findings: vec![
                // Reload-safe, has value → should appear.
                make_config_finding(
                    ConfigFindingKind::WorkMemTooLow,
                    "work_mem",
                    Some("4MB"),
                    false,
                    Severity::Warning,
                    EvidenceClass::Heuristic,
                ),
                // Requires restart → should be excluded.
                make_config_finding(
                    ConfigFindingKind::RestartRequiredGuc,
                    "wal_buffers",
                    Some("16MB"),
                    true,
                    Severity::Info,
                    EvidenceClass::Heuristic,
                ),
                // No recommended value → should be excluded.
                make_config_finding(
                    ConfigFindingKind::RandomPageCostTooHigh,
                    "random_page_cost",
                    None,
                    false,
                    Severity::Critical,
                    EvidenceClass::Advisory,
                ),
                // Reload-safe, has value → should appear.
                make_config_finding(
                    ConfigFindingKind::CheckpointCompletionTargetLow,
                    "checkpoint_completion_target",
                    Some("0.9"),
                    false,
                    Severity::Warning,
                    EvidenceClass::Heuristic,
                ),
            ],
        };
        let proposals = report.to_proposals();
        assert_eq!(proposals.len(), 2, "expected 2 actionable proposals");
        let actions: Vec<_> = proposals
            .iter()
            .map(|p| p.proposed_action.as_str())
            .collect();
        assert!(
            actions.iter().any(|a| a.contains("work_mem")),
            "work_mem proposal should be present"
        );
        assert!(
            actions
                .iter()
                .any(|a| a.contains("checkpoint_completion_target")),
            "checkpoint_completion_target proposal should be present"
        );
    }

    #[test]
    fn to_proposals_empty_when_all_restart_required() {
        let report = ConfigTuningReport {
            findings: vec![
                make_config_finding(
                    ConfigFindingKind::RestartRequiredGuc,
                    "max_connections",
                    None,
                    true,
                    Severity::Info,
                    EvidenceClass::Heuristic,
                ),
                make_config_finding(
                    ConfigFindingKind::RestartRequiredGuc,
                    "wal_buffers",
                    Some("16MB"),
                    true,
                    Severity::Info,
                    EvidenceClass::Heuristic,
                ),
            ],
        };
        let proposals = report.to_proposals();
        assert!(
            proposals.is_empty(),
            "all restart-required findings → no proposals"
        );
    }
}
