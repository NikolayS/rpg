//! Builds a [`ConnectorRegistry`] from the `[connectors]` section of the
//! loaded config.
//!
//! Each connector section is optional; when absent the connector is skipped.
//! When present, the env vars named in the config are read at startup.  If a
//! required env var is missing the connector is logged as disabled and
//! excluded from the registry.

use crate::config::ConnectorsConfig;
use crate::connectors::ConnectorRegistry;

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Build a [`ConnectorRegistry`] from a [`ConnectorsConfig`].
///
/// For each connector that is configured and has the required env vars set,
/// the connector is instantiated and registered.  Missing or empty env vars
/// cause the connector to be skipped with a warning log.
pub fn build_connector_registry(config: &ConnectorsConfig) -> ConnectorRegistry {
    let mut registry = ConnectorRegistry::new();

    register_datadog(config, &mut registry);
    register_pganalyze(config, &mut registry);
    register_cloudwatch(config, &mut registry);
    register_postgresai(config, &mut registry);
    register_supabase(config, &mut registry);
    register_github(config, &mut registry);
    register_gitlab(config, &mut registry);
    register_jira(config, &mut registry);

    // Log a summary of which connectors are active.
    let active: Vec<&str> = registry.list().iter().map(|c| c.id()).collect();
    if active.is_empty() {
        crate::logging::info("connectors", "Connectors: none enabled");
    } else {
        crate::logging::info("connectors", &format!("Connectors: {}", active.join(", ")));
    }

    registry
}

// ---------------------------------------------------------------------------
// Per-connector helpers
// ---------------------------------------------------------------------------

fn register_datadog(config: &ConnectorsConfig, registry: &mut ConnectorRegistry) {
    let Some(cfg) = &config.datadog else {
        crate::logging::info("connectors", "datadog: not configured");
        return;
    };
    if !cfg.enabled {
        crate::logging::info("connectors", "datadog: disabled");
        return;
    }
    let api_key = match read_env(&cfg.api_key_env, "datadog") {
        Some(v) => v,
        None => return,
    };
    let app_key = match read_env(&cfg.app_key_env, "datadog") {
        Some(v) => v,
        None => return,
    };
    registry.register(Box::new(crate::connectors::datadog::DatadogConnector::new(
        api_key, app_key,
    )));
    crate::logging::info("connectors", "datadog: enabled");
}

fn register_pganalyze(config: &ConnectorsConfig, registry: &mut ConnectorRegistry) {
    let Some(cfg) = &config.pganalyze else {
        crate::logging::info("connectors", "pganalyze: not configured");
        return;
    };
    if !cfg.enabled {
        crate::logging::info("connectors", "pganalyze: disabled");
        return;
    }
    let api_key = match read_env(&cfg.api_key_env, "pganalyze") {
        Some(v) => v,
        None => return,
    };
    registry.register(Box::new(
        crate::connectors::pganalyze::PganalyzeConnector::new(api_key),
    ));
    crate::logging::info("connectors", "pganalyze: enabled");
}

fn register_cloudwatch(config: &ConnectorsConfig, registry: &mut ConnectorRegistry) {
    let Some(cfg) = &config.cloudwatch else {
        crate::logging::info("connectors", "cloudwatch: not configured");
        return;
    };
    if !cfg.enabled {
        crate::logging::info("connectors", "cloudwatch: disabled");
        return;
    }
    let region = match read_env(&cfg.region_env, "cloudwatch") {
        Some(v) => v,
        None => return,
    };
    let access_key_id = match read_env(&cfg.access_key_id_env, "cloudwatch") {
        Some(v) => v,
        None => return,
    };
    let secret_access_key = match read_env(&cfg.secret_access_key_env, "cloudwatch") {
        Some(v) => v,
        None => return,
    };
    registry.register(Box::new(
        crate::connectors::cloudwatch::CloudWatchConnector::new(
            region,
            access_key_id,
            secret_access_key,
        ),
    ));
    crate::logging::info("connectors", "cloudwatch: enabled");
}

fn register_postgresai(config: &ConnectorsConfig, registry: &mut ConnectorRegistry) {
    let Some(cfg) = &config.postgresai else {
        crate::logging::info("connectors", "postgresai: not configured");
        return;
    };
    if !cfg.enabled {
        crate::logging::info("connectors", "postgresai: disabled");
        return;
    }
    let api_key = match read_env(&cfg.api_key_env, "postgresai") {
        Some(v) => v,
        None => return,
    };
    registry.register(Box::new(
        crate::connectors::postgresai::PostgresAIConnector::new(api_key),
    ));
    crate::logging::info("connectors", "postgresai: enabled");
}

fn register_supabase(config: &ConnectorsConfig, registry: &mut ConnectorRegistry) {
    let Some(cfg) = &config.supabase else {
        crate::logging::info("connectors", "supabase: not configured");
        return;
    };
    if !cfg.enabled {
        crate::logging::info("connectors", "supabase: disabled");
        return;
    }
    let token = match read_env(&cfg.access_token_env, "supabase") {
        Some(v) => v,
        None => return,
    };
    registry.register(Box::new(
        crate::connectors::supabase::SupabaseConnector::new(token),
    ));
    crate::logging::info("connectors", "supabase: enabled");
}

fn register_github(config: &ConnectorsConfig, registry: &mut ConnectorRegistry) {
    let Some(cfg) = &config.github else {
        crate::logging::info("connectors", "github: not configured");
        return;
    };
    if !cfg.enabled {
        crate::logging::info("connectors", "github: disabled");
        return;
    }
    let token = match read_env(&cfg.token_env, "github") {
        Some(v) => v,
        None => return,
    };
    let repo = match &cfg.repo {
        Some(r) => r.clone(),
        None => {
            crate::logging::warn(
                "connectors",
                "github: skipped — 'repo' not set in [connectors.github]",
            );
            return;
        }
    };
    let (owner, repo_name) = match repo.split_once('/') {
        Some((o, r)) => (o.to_owned(), r.to_owned()),
        None => {
            crate::logging::warn(
                "connectors",
                "github: skipped — 'repo' must be in 'owner/repo' format",
            );
            return;
        }
    };
    registry.register(Box::new(crate::connectors::github::GitHubConnector::new(
        token, owner, repo_name,
    )));
    crate::logging::info("connectors", "github: enabled");
}

fn register_gitlab(config: &ConnectorsConfig, registry: &mut ConnectorRegistry) {
    let Some(cfg) = &config.gitlab else {
        crate::logging::info("connectors", "gitlab: not configured");
        return;
    };
    if !cfg.enabled {
        crate::logging::info("connectors", "gitlab: disabled");
        return;
    }
    let token = match read_env(&cfg.token_env, "gitlab") {
        Some(v) => v,
        None => return,
    };
    let project_id = match &cfg.project_id {
        Some(p) => p.clone(),
        None => {
            crate::logging::warn(
                "connectors",
                "gitlab: skipped — 'project_id' not set in [connectors.gitlab]",
            );
            return;
        }
    };
    registry.register(Box::new(crate::connectors::gitlab::GitLabConnector::new(
        token, project_id,
    )));
    crate::logging::info("connectors", "gitlab: enabled");
}

fn register_jira(config: &ConnectorsConfig, registry: &mut ConnectorRegistry) {
    let Some(cfg) = &config.jira else {
        crate::logging::info("connectors", "jira: not configured");
        return;
    };
    if !cfg.enabled {
        crate::logging::info("connectors", "jira: disabled");
        return;
    }
    let email = match read_env(&cfg.email_env, "jira") {
        Some(v) => v,
        None => return,
    };
    let api_token = match read_env(&cfg.api_token_env, "jira") {
        Some(v) => v,
        None => return,
    };
    let mut connector = crate::connectors::jira::JiraConnector::new(email, api_token);
    if let Some(ref url) = cfg.base_url {
        connector = connector.with_base_url(url.clone());
    }
    registry.register(Box::new(connector));
    crate::logging::info("connectors", "jira: enabled");
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Read an environment variable.  Logs a warning and returns `None` when the
/// variable is absent or empty.
fn read_env(name: &str, connector: &str) -> Option<String> {
    match std::env::var(name) {
        Ok(v) if !v.is_empty() => Some(v),
        _ => {
            crate::logging::warn(
                "connectors",
                &format!("{connector}: skipped — env var {name} not set"),
            );
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ConnectorsConfig, DatadogConfig, PganalyzeConfig};

    #[test]
    fn empty_connectors_config_builds_empty_registry() {
        let cfg = ConnectorsConfig::default();
        let registry = build_connector_registry(&cfg);
        assert!(registry.list().is_empty());
    }

    #[test]
    fn disabled_datadog_not_registered() {
        let cfg = ConnectorsConfig {
            datadog: Some(DatadogConfig {
                enabled: false,
                ..Default::default()
            }),
            ..Default::default()
        };
        let registry = build_connector_registry(&cfg);
        assert!(registry.get("datadog").is_none());
    }

    #[test]
    fn datadog_registered_when_env_vars_set() {
        // Use unique env var names to avoid clobbering other tests.
        std::env::set_var("TEST_DD_API_KEY", "test-api-key");
        std::env::set_var("TEST_DD_APP_KEY", "test-app-key");

        let cfg = ConnectorsConfig {
            datadog: Some(DatadogConfig {
                enabled: true,
                api_key_env: "TEST_DD_API_KEY".to_owned(),
                app_key_env: "TEST_DD_APP_KEY".to_owned(),
            }),
            ..Default::default()
        };
        let registry = build_connector_registry(&cfg);
        assert!(registry.get("datadog").is_some());

        std::env::remove_var("TEST_DD_API_KEY");
        std::env::remove_var("TEST_DD_APP_KEY");
    }

    #[test]
    fn datadog_skipped_when_api_key_missing() {
        std::env::remove_var("TEST_DD_API_KEY_MISSING");
        let cfg = ConnectorsConfig {
            datadog: Some(DatadogConfig {
                enabled: true,
                api_key_env: "TEST_DD_API_KEY_MISSING".to_owned(),
                app_key_env: "TEST_DD_APP_KEY_MISSING".to_owned(),
            }),
            ..Default::default()
        };
        let registry = build_connector_registry(&cfg);
        assert!(registry.get("datadog").is_none());
    }

    #[test]
    fn pganalyze_registered_when_env_var_set() {
        std::env::set_var("TEST_PGANALYZE_API_KEY", "pg-test-key");
        let cfg = ConnectorsConfig {
            pganalyze: Some(PganalyzeConfig {
                enabled: true,
                api_key_env: "TEST_PGANALYZE_API_KEY".to_owned(),
            }),
            ..Default::default()
        };
        let registry = build_connector_registry(&cfg);
        assert!(registry.get("pganalyze").is_some());
        std::env::remove_var("TEST_PGANALYZE_API_KEY");
    }

    #[test]
    fn pganalyze_skipped_when_disabled() {
        let cfg = ConnectorsConfig {
            pganalyze: Some(PganalyzeConfig {
                enabled: false,
                ..Default::default()
            }),
            ..Default::default()
        };
        let registry = build_connector_registry(&cfg);
        assert!(registry.get("pganalyze").is_none());
    }

    #[test]
    fn github_skipped_without_repo() {
        use crate::config::GitHubConfig;
        std::env::set_var("TEST_GH_TOKEN", "ghp_test");
        let cfg = ConnectorsConfig {
            github: Some(GitHubConfig {
                enabled: true,
                token_env: "TEST_GH_TOKEN".to_owned(),
                repo: None,
            }),
            ..Default::default()
        };
        let registry = build_connector_registry(&cfg);
        assert!(registry.get("github").is_none());
        std::env::remove_var("TEST_GH_TOKEN");
    }

    #[test]
    fn github_skipped_with_malformed_repo() {
        use crate::config::GitHubConfig;
        std::env::set_var("TEST_GH_TOKEN2", "ghp_test2");
        let cfg = ConnectorsConfig {
            github: Some(GitHubConfig {
                enabled: true,
                token_env: "TEST_GH_TOKEN2".to_owned(),
                // Missing the slash — not owner/repo format.
                repo: Some("justarepo".to_owned()),
            }),
            ..Default::default()
        };
        let registry = build_connector_registry(&cfg);
        assert!(registry.get("github").is_none());
        std::env::remove_var("TEST_GH_TOKEN2");
    }
}
