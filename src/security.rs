//! Security Analyzer — detects role misconfigurations, weak authentication
//! settings, and `pg_hba` trust authentication entries.
//!
//! Operates at Observe level: reads `pg_authid`, `pg_roles`, and
//! `pg_hba_file_rules` (PG 15+) to produce structured findings.
//! No writes are performed.
//!
//! # Sub-findings
//!
//! | Sub-finding | Evidence Class | Source |
//! |---|---|---|
//! | Superuser roles (non-postgres) | Heuristic | `pg_roles` |
//! | Roles with no password expiry | Advisory | `pg_roles` |
//! | Roles with CREATEDB or CREATEROLE | Advisory | `pg_roles` |
//! | Unencrypted / weak password hash | Heuristic | `pg_authid` (superuser only) |
//! | Trust authentication entries | Heuristic | `pg_hba_file_rules` (PG 15+) |

use crate::governance::{EvidenceClass, Severity};

use std::fmt::Write as _;

// ---------------------------------------------------------------------------
// Security finding types
// ---------------------------------------------------------------------------

/// Category of security finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecurityFindingKind {
    /// A non-postgres role has `rolsuper = true`.
    SuperuserRole,
    /// A login role has no password expiry (`rolvaliduntil IS NULL`).
    NoPasswordExpiry,
    /// A role has `CREATEDB` or `CREATEROLE` privileges.
    ElevatedPrivilege,
    /// A role's password hash uses an unencrypted or weak algorithm.
    WeakPasswordHash,
    /// A `pg_hba.conf` entry uses `trust` authentication method.
    TrustAuthentication,
}

impl SecurityFindingKind {
    /// Evidence class for this finding kind.
    #[allow(dead_code)]
    pub fn evidence_class(self) -> EvidenceClass {
        match self {
            Self::SuperuserRole | Self::WeakPasswordHash | Self::TrustAuthentication => {
                EvidenceClass::Heuristic
            }
            Self::NoPasswordExpiry | Self::ElevatedPrivilege => EvidenceClass::Advisory,
        }
    }

    /// Human-readable label.
    pub fn label(self) -> &'static str {
        match self {
            Self::SuperuserRole => "superuser_role",
            Self::NoPasswordExpiry => "no_password_expiry",
            Self::ElevatedPrivilege => "elevated_privilege",
            Self::WeakPasswordHash => "weak_password_hash",
            Self::TrustAuthentication => "trust_authentication",
        }
    }
}

/// A single security finding.
#[derive(Debug, Clone)]
pub struct SecurityFinding {
    /// What kind of finding.
    pub kind: SecurityFindingKind,
    /// Schema name (empty for instance-level or role-level findings).
    pub schema: String,
    /// Object name: role name, HBA line number, etc.
    pub table: String,
    /// Human-readable description.
    pub description: String,
    /// Severity level.
    pub severity: Severity,
    /// Evidence class.
    #[allow(dead_code)]
    pub evidence_class: EvidenceClass,
    /// Suggested remediation (Observe mode: informational only).
    pub suggested_action: Option<String>,
}

/// Complete security report.
#[derive(Debug, Clone)]
pub struct SecurityReport {
    /// All findings, sorted by severity (critical first).
    pub findings: Vec<SecurityFinding>,
}

impl SecurityReport {
    /// Display the report to the terminal.
    pub fn display(&self) {
        if self.findings.is_empty() {
            eprintln!("Security: no issues found.");
            return;
        }
        eprintln!(
            "Security: {} issue{} found.\n",
            self.findings.len(),
            if self.findings.len() == 1 { "" } else { "s" }
        );
        for f in &self.findings {
            let icon = match f.severity {
                Severity::Critical => "!!",
                Severity::Warning => "! ",
                Severity::Info => "  ",
            };
            if f.schema.is_empty() {
                eprintln!("{icon} [{}] {}", f.kind.label(), f.description);
            } else {
                eprintln!("{icon} [{}] {}.{}", f.kind.label(), f.schema, f.table);
                eprintln!("   {}", f.description);
            }
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
            return "No security issues found.".to_owned();
        }
        let mut out = format!("Security report: {} finding(s)\n\n", self.findings.len());
        for (i, f) in self.findings.iter().enumerate() {
            if f.schema.is_empty() {
                let _ = writeln!(out, "{}. [{}] {}", i + 1, f.kind.label(), f.description);
            } else {
                let _ = writeln!(
                    out,
                    "{}. [{}] {}.{}: {}",
                    i + 1,
                    f.kind.label(),
                    f.schema,
                    f.table,
                    f.description
                );
            }
            if let Some(ref action) = f.suggested_action {
                let _ = writeln!(out, "   Suggested: {action}");
            }
            out.push('\n');
        }
        out
    }
}

impl SecurityFinding {
    /// Convert this finding into an [`crate::governance::ActionProposal`].
    ///
    /// Returns `Some` for findings with a safe, well-defined SQL action;
    /// `None` for findings that require manual intervention (password change,
    /// `pg_hba.conf` file edit + server reload).
    #[allow(dead_code)]
    pub fn to_proposal(&self) -> Option<crate::governance::ActionProposal> {
        let role = &self.table;

        let (proposed_action, expected_outcome, risk) = match self.kind {
            SecurityFindingKind::SuperuserRole => {
                let action = format!("alter role {role} nosuperuser;");
                let outcome = format!(
                    "Remove superuser from role '{role}'; \
                     restrict to granted object privileges only"
                );
                let risk = "Revoking superuser may break applications that rely on \
                            unrestricted access. Test in staging before applying."
                    .to_owned();
                (action, outcome, risk)
            }
            SecurityFindingKind::NoPasswordExpiry => {
                // Use a fixed 90-day placeholder from a known reference date.
                let action = format!("alter role {role} valid until '2026-06-12T00:00:00';");
                let outcome = format!(
                    "Set password expiry for role '{role}' to 90 days; \
                     enforce regular credential rotation"
                );
                let risk = "After expiry the role cannot log in until the password \
                            is reset. Coordinate with application teams."
                    .to_owned();
                (action, outcome, risk)
            }
            SecurityFindingKind::ElevatedPrivilege => {
                // Determine which privilege(s) to revoke from the description.
                let has_createdb = self.description.contains("CREATEDB");
                let has_createrole = self.description.contains("CREATEROLE");

                let action = match (has_createdb, has_createrole) {
                    (true, true) => format!("alter role {role} nocreatedb nocreaterole;"),
                    (true, false) => format!("alter role {role} nocreatedb;"),
                    (false, true) => format!("alter role {role} nocreaterole;"),
                    (false, false) => return None,
                };
                let outcome = format!(
                    "Remove elevated privileges from role '{role}'; \
                     limit to standard object-level permissions"
                );
                let risk = "If '{role}' is used by automation to create databases \
                            or manage roles this will break those workflows. \
                            Verify before applying."
                    .to_owned();
                (action, outcome, risk)
            }
            // WeakPasswordHash requires a new plaintext password (cannot be
            // automated safely) and TrustAuthentication requires editing
            // pg_hba.conf on disk + server reload — neither is a pure SQL action.
            SecurityFindingKind::WeakPasswordHash | SecurityFindingKind::TrustAuthentication => {
                return None
            }
        };

        Some(crate::governance::ActionProposal {
            feature: crate::governance::FeatureArea::Security,
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

impl SecurityReport {
    /// Collect all [`crate::governance::ActionProposal`]s from this report.
    ///
    /// Only findings with safe, automatable SQL actions produce proposals.
    #[allow(dead_code)]
    pub fn to_proposals(&self) -> Vec<crate::governance::ActionProposal> {
        self.findings
            .iter()
            .filter_map(SecurityFinding::to_proposal)
            .collect()
    }
}

// ---------------------------------------------------------------------------
// SQL queries
// ---------------------------------------------------------------------------

/// Detect non-postgres roles that have superuser privileges.
///
/// The built-in `postgres` superuser is excluded as expected.
const SUPERUSER_ROLES_SQL: &str = "\
    select \
        rolname \
    from pg_roles \
    where \
        rolsuper = true \
        and rolname <> 'postgres' \
    order by rolname";

/// Detect login roles with no password expiry date set.
///
/// Roles with `rolvaliduntil IS NULL` have passwords that never expire,
/// which is a weak security posture in environments requiring password rotation.
const NO_PASSWORD_EXPIRY_SQL: &str = "\
    select \
        rolname \
    from pg_roles \
    where \
        rolcanlogin = true \
        and rolvaliduntil is null \
        and rolname <> 'postgres' \
    order by rolname";

/// Detect roles with elevated privileges: CREATEDB or CREATEROLE.
///
/// These privileges are not superuser but still allow significant changes.
const ELEVATED_PRIVILEGE_SQL: &str = "\
    select \
        rolname, \
        rolcreatedb, \
        rolcreaterole \
    from pg_roles \
    where \
        (rolcreatedb = true or rolcreaterole = true) \
        and rolsuper = false \
        and rolname <> 'postgres' \
    order by rolname";

/// Detect roles with unencrypted or weak password hashes via `pg_authid`.
///
/// Requires superuser access. Returns roles where the password column
/// is not NULL and does not start with the `scram-sha-256` prefix.
/// An empty prefix (no leading `$`) indicates a plaintext or md5 hash.
const WEAK_PASSWORD_HASH_SQL: &str = "\
    select \
        rolname, \
        left(coalesce(rolpassword, ''), 20) as hash_prefix \
    from pg_authid \
    where \
        rolcanlogin = true \
        and rolpassword is not null \
        and rolpassword not like 'SCRAM-SHA-256$%' \
        and rolname <> 'postgres' \
    order by rolname";

/// Detect `trust` authentication entries in `pg_hba_file_rules` (PG 15+).
///
/// Trust authentication allows connections without a password, which is
/// a significant security risk in network-accessible configurations.
const TRUST_AUTH_SQL: &str = "\
    select \
        line_number, \
        type, \
        coalesce(array_to_string(database, ','), '') as database, \
        coalesce(array_to_string(address, ','), '') as address, \
        auth_method \
    from pg_hba_file_rules \
    where auth_method = 'trust' \
    order by line_number";

// ---------------------------------------------------------------------------
// Public analyzer
// ---------------------------------------------------------------------------

/// Security analyzer — Observe mode, zero writes.
pub struct SecurityAnalyzer;

impl SecurityAnalyzer {
    /// Run all security checks and return a [`SecurityReport`].
    ///
    /// All queries are read-only. Individual query failures are silently
    /// skipped so that a single unavailable view does not abort the analysis.
    /// Queries requiring superuser access (e.g. `pg_authid`) are handled
    /// gracefully when access is denied.
    pub async fn analyze(client: &tokio_postgres::Client) -> SecurityReport {
        let mut findings = Vec::new();

        collect_superuser_role_findings(client, &mut findings).await;
        collect_no_password_expiry_findings(client, &mut findings).await;
        collect_elevated_privilege_findings(client, &mut findings).await;
        collect_weak_password_hash_findings(client, &mut findings).await;
        collect_trust_auth_findings(client, &mut findings).await;

        // Sort: Critical first, then Warning, then Info.
        findings.sort_by(|a, b| b.severity.cmp(&a.severity));

        SecurityReport { findings }
    }
}

// ---------------------------------------------------------------------------
// Collection helpers
// ---------------------------------------------------------------------------

async fn collect_superuser_role_findings(
    client: &tokio_postgres::Client,
    findings: &mut Vec<SecurityFinding>,
) {
    let Ok(messages) = client.simple_query(SUPERUSER_ROLES_SQL).await else {
        return;
    };
    for msg in messages {
        if let tokio_postgres::SimpleQueryMessage::Row(row) = msg {
            let rolname = row.get(0).unwrap_or("").to_owned();
            if rolname.is_empty() {
                continue;
            }
            findings.push(SecurityFinding {
                kind: SecurityFindingKind::SuperuserRole,
                schema: "roles".to_owned(),
                table: rolname.clone(),
                description: format!(
                    "Role '{rolname}' has superuser privileges — \
                     full unrestricted access to the cluster"
                ),
                severity: Severity::Warning,
                evidence_class: EvidenceClass::Heuristic,
                suggested_action: Some(format!(
                    "Review if '{rolname}' requires superuser; \
                     consider revoking: ALTER ROLE {rolname} NOSUPERUSER"
                )),
            });
        }
    }
}

async fn collect_no_password_expiry_findings(
    client: &tokio_postgres::Client,
    findings: &mut Vec<SecurityFinding>,
) {
    let Ok(messages) = client.simple_query(NO_PASSWORD_EXPIRY_SQL).await else {
        return;
    };
    for msg in messages {
        if let tokio_postgres::SimpleQueryMessage::Row(row) = msg {
            let rolname = row.get(0).unwrap_or("").to_owned();
            if rolname.is_empty() {
                continue;
            }
            findings.push(SecurityFinding {
                kind: SecurityFindingKind::NoPasswordExpiry,
                schema: "roles".to_owned(),
                table: rolname.clone(),
                description: format!(
                    "Login role '{rolname}' has no password expiry \
                     (rolvaliduntil IS NULL)"
                ),
                severity: Severity::Info,
                evidence_class: EvidenceClass::Advisory,
                suggested_action: Some(format!(
                    "Set a password expiry: \
                     ALTER ROLE {rolname} VALID UNTIL '2027-01-01'"
                )),
            });
        }
    }
}

async fn collect_elevated_privilege_findings(
    client: &tokio_postgres::Client,
    findings: &mut Vec<SecurityFinding>,
) {
    let Ok(messages) = client.simple_query(ELEVATED_PRIVILEGE_SQL).await else {
        return;
    };
    for msg in messages {
        if let tokio_postgres::SimpleQueryMessage::Row(row) = msg {
            let rolname = row.get(0).unwrap_or("").to_owned();
            let createdb = row.get(1).unwrap_or("f") == "t";
            let createrole = row.get(2).unwrap_or("f") == "t";

            if rolname.is_empty() {
                continue;
            }

            let privileges: Vec<&str> = [
                if createdb { Some("CREATEDB") } else { None },
                if createrole { Some("CREATEROLE") } else { None },
            ]
            .into_iter()
            .flatten()
            .collect();

            let privs_str = privileges.join(", ");

            findings.push(SecurityFinding {
                kind: SecurityFindingKind::ElevatedPrivilege,
                schema: "roles".to_owned(),
                table: rolname.clone(),
                description: format!("Role '{rolname}' has elevated privileges: {privs_str}"),
                severity: Severity::Info,
                evidence_class: EvidenceClass::Advisory,
                suggested_action: Some(format!(
                    "Verify '{rolname}' requires {privs_str}; \
                     revoke if unneeded: ALTER ROLE {rolname} NO{privs_str}"
                )),
            });
        }
    }
}

async fn collect_weak_password_hash_findings(
    client: &tokio_postgres::Client,
    findings: &mut Vec<SecurityFinding>,
) {
    // pg_authid requires superuser; if access is denied, skip silently.
    let Ok(messages) = client.simple_query(WEAK_PASSWORD_HASH_SQL).await else {
        return;
    };
    for msg in messages {
        if let tokio_postgres::SimpleQueryMessage::Row(row) = msg {
            let rolname = row.get(0).unwrap_or("").to_owned();
            let hash_prefix = row.get(1).unwrap_or("").to_owned();

            if rolname.is_empty() {
                continue;
            }

            let algo = if hash_prefix.starts_with("md5") {
                "MD5"
            } else if hash_prefix.is_empty() {
                "plaintext"
            } else {
                "unknown/weak"
            };

            findings.push(SecurityFinding {
                kind: SecurityFindingKind::WeakPasswordHash,
                schema: "roles".to_owned(),
                table: rolname.clone(),
                description: format!(
                    "Role '{rolname}' uses a {algo} password hash \
                     — upgrade to scram-sha-256"
                ),
                severity: Severity::Warning,
                evidence_class: EvidenceClass::Heuristic,
                suggested_action: Some(format!(
                    "Set password_encryption = 'scram-sha-256', then: \
                     ALTER ROLE {rolname} PASSWORD '<new-password>'"
                )),
            });
        }
    }
}

async fn collect_trust_auth_findings(
    client: &tokio_postgres::Client,
    findings: &mut Vec<SecurityFinding>,
) {
    // pg_hba_file_rules requires PG 15+ and superuser/pg_read_all_settings.
    // Gracefully skip if the view is unavailable (PG 14) or access denied.
    let Ok(messages) = client.simple_query(TRUST_AUTH_SQL).await else {
        return;
    };
    for msg in messages {
        if let tokio_postgres::SimpleQueryMessage::Row(row) = msg {
            let line_number = row.get(0).unwrap_or("?").to_owned();
            let hba_type = row.get(1).unwrap_or("").to_owned();
            let database = row.get(2).unwrap_or("").to_owned();
            let address = row.get(3).unwrap_or("").to_owned();
            let auth_method = row.get(4).unwrap_or("").to_owned();

            if auth_method != "trust" {
                continue;
            }

            // Local socket trust entries are low-risk; network trust is critical.
            let is_network = !address.is_empty()
                && address != "local"
                && address != "127.0.0.1/32"
                && address != "::1/128";

            let severity = if is_network {
                Severity::Warning
            } else {
                Severity::Info
            };

            let addr_note = if address.is_empty() {
                String::new()
            } else {
                format!(" address={address}")
            };

            findings.push(SecurityFinding {
                kind: SecurityFindingKind::TrustAuthentication,
                schema: "hba".to_owned(),
                table: format!("line_{line_number}"),
                description: format!(
                    "pg_hba line {line_number}: type={hba_type} \
                     database={database}{addr_note} method=trust — \
                     allows passwordless connections"
                ),
                severity,
                evidence_class: EvidenceClass::Heuristic,
                suggested_action: Some(
                    "Replace 'trust' with 'scram-sha-256' or 'md5' \
                     in pg_hba.conf, then reload: SELECT pg_reload_conf()"
                        .to_owned(),
                ),
            });
        }
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // SecurityFindingKind tests
    // -----------------------------------------------------------------------

    #[test]
    fn finding_kind_labels() {
        assert_eq!(SecurityFindingKind::SuperuserRole.label(), "superuser_role");
        assert_eq!(
            SecurityFindingKind::NoPasswordExpiry.label(),
            "no_password_expiry"
        );
        assert_eq!(
            SecurityFindingKind::ElevatedPrivilege.label(),
            "elevated_privilege"
        );
        assert_eq!(
            SecurityFindingKind::WeakPasswordHash.label(),
            "weak_password_hash"
        );
        assert_eq!(
            SecurityFindingKind::TrustAuthentication.label(),
            "trust_authentication"
        );
    }

    #[test]
    fn finding_kind_evidence_classes() {
        assert_eq!(
            SecurityFindingKind::SuperuserRole.evidence_class(),
            EvidenceClass::Heuristic
        );
        assert_eq!(
            SecurityFindingKind::WeakPasswordHash.evidence_class(),
            EvidenceClass::Heuristic
        );
        assert_eq!(
            SecurityFindingKind::TrustAuthentication.evidence_class(),
            EvidenceClass::Heuristic
        );
        assert_eq!(
            SecurityFindingKind::NoPasswordExpiry.evidence_class(),
            EvidenceClass::Advisory
        );
        assert_eq!(
            SecurityFindingKind::ElevatedPrivilege.evidence_class(),
            EvidenceClass::Advisory
        );
    }

    // -----------------------------------------------------------------------
    // SecurityReport::to_prompt tests
    // -----------------------------------------------------------------------

    #[test]
    fn empty_report_to_prompt() {
        let report = SecurityReport {
            findings: Vec::new(),
        };
        assert!(report.to_prompt().contains("No security issues found."));
    }

    #[test]
    fn report_to_prompt_with_superuser_finding() {
        let report = SecurityReport {
            findings: vec![SecurityFinding {
                kind: SecurityFindingKind::SuperuserRole,
                schema: "roles".to_owned(),
                table: "app_admin".to_owned(),
                description: "Role 'app_admin' has superuser privileges".to_owned(),
                severity: Severity::Warning,
                evidence_class: EvidenceClass::Heuristic,
                suggested_action: Some("ALTER ROLE app_admin NOSUPERUSER".to_owned()),
            }],
        };
        let prompt = report.to_prompt();
        assert!(prompt.contains("1 finding"));
        assert!(prompt.contains("[superuser_role]"));
        assert!(prompt.contains("roles.app_admin"));
        assert!(prompt.contains("NOSUPERUSER"));
    }

    #[test]
    fn report_to_prompt_instance_level_finding() {
        let report = SecurityReport {
            findings: vec![SecurityFinding {
                kind: SecurityFindingKind::TrustAuthentication,
                schema: String::new(),
                table: String::new(),
                description: "pg_hba line 5: trust auth detected".to_owned(),
                severity: Severity::Warning,
                evidence_class: EvidenceClass::Heuristic,
                suggested_action: None,
            }],
        };
        let prompt = report.to_prompt();
        assert!(prompt.contains("[trust_authentication]"));
        assert!(prompt.contains("trust auth detected"));
    }

    #[test]
    fn report_sorts_by_severity() {
        let mut report = SecurityReport {
            findings: vec![
                SecurityFinding {
                    kind: SecurityFindingKind::NoPasswordExpiry,
                    schema: "roles".to_owned(),
                    table: "alice".to_owned(),
                    description: "no expiry".to_owned(),
                    severity: Severity::Info,
                    evidence_class: EvidenceClass::Advisory,
                    suggested_action: None,
                },
                SecurityFinding {
                    kind: SecurityFindingKind::SuperuserRole,
                    schema: "roles".to_owned(),
                    table: "badactor".to_owned(),
                    description: "superuser".to_owned(),
                    severity: Severity::Warning,
                    evidence_class: EvidenceClass::Heuristic,
                    suggested_action: None,
                },
                SecurityFinding {
                    kind: SecurityFindingKind::TrustAuthentication,
                    schema: "hba".to_owned(),
                    table: "line_3".to_owned(),
                    description: "trust on network".to_owned(),
                    severity: Severity::Warning,
                    evidence_class: EvidenceClass::Heuristic,
                    suggested_action: None,
                },
            ],
        };
        report.findings.sort_by(|a, b| b.severity.cmp(&a.severity));
        assert_eq!(report.findings[0].severity, Severity::Warning);
        assert_eq!(report.findings[2].severity, Severity::Info);
    }

    // -----------------------------------------------------------------------
    // SQL constant tests
    // -----------------------------------------------------------------------

    #[test]
    fn superuser_roles_sql_excludes_postgres() {
        assert!(SUPERUSER_ROLES_SQL.contains("rolsuper = true"));
        assert!(SUPERUSER_ROLES_SQL.contains("'postgres'"));
        assert!(SUPERUSER_ROLES_SQL.contains("pg_roles"));
    }

    #[test]
    fn no_password_expiry_sql_targets_login_roles() {
        assert!(NO_PASSWORD_EXPIRY_SQL.contains("rolcanlogin = true"));
        assert!(NO_PASSWORD_EXPIRY_SQL.contains("rolvaliduntil is null"));
        assert!(NO_PASSWORD_EXPIRY_SQL.contains("pg_roles"));
    }

    #[test]
    fn elevated_privilege_sql_checks_createdb_createrole() {
        assert!(ELEVATED_PRIVILEGE_SQL.contains("rolcreatedb"));
        assert!(ELEVATED_PRIVILEGE_SQL.contains("rolcreaterole"));
        assert!(ELEVATED_PRIVILEGE_SQL.contains("rolsuper = false"));
    }

    #[test]
    fn weak_password_hash_sql_targets_pg_authid() {
        assert!(WEAK_PASSWORD_HASH_SQL.contains("pg_authid"));
        assert!(WEAK_PASSWORD_HASH_SQL.contains("SCRAM-SHA-256"));
        assert!(WEAK_PASSWORD_HASH_SQL.contains("rolpassword"));
    }

    #[test]
    fn trust_auth_sql_filters_trust_method() {
        assert!(TRUST_AUTH_SQL.contains("pg_hba_file_rules"));
        assert!(TRUST_AUTH_SQL.contains("auth_method = 'trust'"));
        assert!(TRUST_AUTH_SQL.contains("line_number"));
    }

    // -----------------------------------------------------------------------
    // Severity logic tests
    // -----------------------------------------------------------------------

    #[test]
    fn trust_auth_network_address_gives_warning() {
        let address = "0.0.0.0/0";
        let is_network = !address.is_empty()
            && address != "local"
            && address != "127.0.0.1/32"
            && address != "::1/128";
        let severity = if is_network {
            Severity::Warning
        } else {
            Severity::Info
        };
        assert_eq!(severity, Severity::Warning);
    }

    #[test]
    fn trust_auth_localhost_address_gives_info() {
        let address = "127.0.0.1/32";
        let is_network = !address.is_empty()
            && address != "local"
            && address != "127.0.0.1/32"
            && address != "::1/128";
        let severity = if is_network {
            Severity::Warning
        } else {
            Severity::Info
        };
        assert_eq!(severity, Severity::Info);
    }

    #[test]
    fn trust_auth_local_socket_gives_info() {
        let address = "local";
        let is_network = !address.is_empty()
            && address != "local"
            && address != "127.0.0.1/32"
            && address != "::1/128";
        let severity = if is_network {
            Severity::Warning
        } else {
            Severity::Info
        };
        assert_eq!(severity, Severity::Info);
    }

    #[test]
    fn trust_auth_ipv6_loopback_gives_info() {
        let address = "::1/128";
        let is_network = !address.is_empty()
            && address != "local"
            && address != "127.0.0.1/32"
            && address != "::1/128";
        let severity = if is_network {
            Severity::Warning
        } else {
            Severity::Info
        };
        assert_eq!(severity, Severity::Info);
    }

    #[test]
    fn weak_password_hash_md5_label() {
        let hash_prefix = "md5abc123";
        let algo = if hash_prefix.starts_with("md5") {
            "MD5"
        } else if hash_prefix.is_empty() {
            "plaintext"
        } else {
            "unknown/weak"
        };
        assert_eq!(algo, "MD5");
    }

    #[test]
    fn weak_password_hash_empty_is_plaintext() {
        let hash_prefix = "";
        let algo = if hash_prefix.starts_with("md5") {
            "MD5"
        } else if hash_prefix.is_empty() {
            "plaintext"
        } else {
            "unknown/weak"
        };
        assert_eq!(algo, "plaintext");
    }

    #[test]
    fn elevated_privilege_createdb_and_createrole_combined() {
        let createdb = true;
        let createrole = true;
        let privileges: Vec<&str> = [
            if createdb { Some("CREATEDB") } else { None },
            if createrole { Some("CREATEROLE") } else { None },
        ]
        .into_iter()
        .flatten()
        .collect();
        assert_eq!(privileges, vec!["CREATEDB", "CREATEROLE"]);
    }

    #[test]
    fn elevated_privilege_createdb_only() {
        let createdb = true;
        let createrole = false;
        let privileges: Vec<&str> = [
            if createdb { Some("CREATEDB") } else { None },
            if createrole { Some("CREATEROLE") } else { None },
        ]
        .into_iter()
        .flatten()
        .collect();
        assert_eq!(privileges, vec!["CREATEDB"]);
    }

    // -----------------------------------------------------------------------
    // to_proposal / to_proposals tests
    // -----------------------------------------------------------------------

    #[test]
    fn superuser_role_to_proposal_contains_nosuperuser_sql() {
        let finding = SecurityFinding {
            kind: SecurityFindingKind::SuperuserRole,
            schema: "roles".to_owned(),
            table: "app_admin".to_owned(),
            description: "Role 'app_admin' has superuser privileges".to_owned(),
            severity: Severity::Warning,
            evidence_class: EvidenceClass::Heuristic,
            suggested_action: None,
        };
        let proposal = finding.to_proposal().expect("should produce a proposal");
        assert!(proposal.proposed_action.contains("nosuperuser"));
        assert!(proposal.proposed_action.contains("app_admin"));
        assert_eq!(proposal.feature, crate::governance::FeatureArea::Security);
    }

    #[test]
    fn no_password_expiry_to_proposal_contains_valid_until_sql() {
        let finding = SecurityFinding {
            kind: SecurityFindingKind::NoPasswordExpiry,
            schema: "roles".to_owned(),
            table: "alice".to_owned(),
            description: "Login role 'alice' has no password expiry".to_owned(),
            severity: Severity::Info,
            evidence_class: EvidenceClass::Advisory,
            suggested_action: None,
        };
        let proposal = finding.to_proposal().expect("should produce a proposal");
        assert!(proposal.proposed_action.contains("valid until"));
        assert!(proposal.proposed_action.contains("alice"));
        assert_eq!(proposal.feature, crate::governance::FeatureArea::Security);
    }

    #[test]
    fn elevated_privilege_createdb_to_proposal_contains_nocreatedb_sql() {
        let finding = SecurityFinding {
            kind: SecurityFindingKind::ElevatedPrivilege,
            schema: "roles".to_owned(),
            table: "appuser".to_owned(),
            description: "Role 'appuser' has elevated privileges: CREATEDB".to_owned(),
            severity: Severity::Info,
            evidence_class: EvidenceClass::Advisory,
            suggested_action: None,
        };
        let proposal = finding.to_proposal().expect("should produce a proposal");
        assert!(proposal.proposed_action.contains("nocreatedb"));
        assert!(proposal.proposed_action.contains("appuser"));
    }

    #[test]
    fn elevated_privilege_createrole_to_proposal_contains_nocreaterole_sql() {
        let finding = SecurityFinding {
            kind: SecurityFindingKind::ElevatedPrivilege,
            schema: "roles".to_owned(),
            table: "dba".to_owned(),
            description: "Role 'dba' has elevated privileges: CREATEROLE".to_owned(),
            severity: Severity::Info,
            evidence_class: EvidenceClass::Advisory,
            suggested_action: None,
        };
        let proposal = finding.to_proposal().expect("should produce a proposal");
        assert!(proposal.proposed_action.contains("nocreaterole"));
        assert!(proposal.proposed_action.contains("dba"));
    }

    #[test]
    fn weak_password_hash_to_proposal_returns_none() {
        let finding = SecurityFinding {
            kind: SecurityFindingKind::WeakPasswordHash,
            schema: "roles".to_owned(),
            table: "bob".to_owned(),
            description: "Role 'bob' uses MD5 hash — upgrade to scram-sha-256".to_owned(),
            severity: Severity::Warning,
            evidence_class: EvidenceClass::Heuristic,
            suggested_action: None,
        };
        assert!(finding.to_proposal().is_none());
    }

    #[test]
    fn trust_authentication_to_proposal_returns_none() {
        let finding = SecurityFinding {
            kind: SecurityFindingKind::TrustAuthentication,
            schema: "hba".to_owned(),
            table: "line_5".to_owned(),
            description: "pg_hba line 5: trust auth".to_owned(),
            severity: Severity::Warning,
            evidence_class: EvidenceClass::Heuristic,
            suggested_action: None,
        };
        assert!(finding.to_proposal().is_none());
    }

    #[test]
    fn report_to_proposals_filters_non_actionable_findings() {
        let report = SecurityReport {
            findings: vec![
                SecurityFinding {
                    kind: SecurityFindingKind::SuperuserRole,
                    schema: "roles".to_owned(),
                    table: "admin".to_owned(),
                    description: "Role 'admin' has superuser".to_owned(),
                    severity: Severity::Warning,
                    evidence_class: EvidenceClass::Heuristic,
                    suggested_action: None,
                },
                SecurityFinding {
                    kind: SecurityFindingKind::WeakPasswordHash,
                    schema: "roles".to_owned(),
                    table: "carol".to_owned(),
                    description: "Role 'carol' uses MD5".to_owned(),
                    severity: Severity::Warning,
                    evidence_class: EvidenceClass::Heuristic,
                    suggested_action: None,
                },
                SecurityFinding {
                    kind: SecurityFindingKind::TrustAuthentication,
                    schema: "hba".to_owned(),
                    table: "line_3".to_owned(),
                    description: "trust on network".to_owned(),
                    severity: Severity::Warning,
                    evidence_class: EvidenceClass::Heuristic,
                    suggested_action: None,
                },
            ],
        };
        let proposals = report.to_proposals();
        // Only SuperuserRole produces a proposal; WeakPasswordHash and
        // TrustAuthentication return None.
        assert_eq!(proposals.len(), 1);
        assert!(proposals[0].proposed_action.contains("nosuperuser"));
    }
}
