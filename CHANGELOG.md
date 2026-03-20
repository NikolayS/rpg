# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [0.3.0] - 2026-03-14

### Added

#### Connectors
- Datadog connector for metric and alert ingestion (#467)
- pganalyze, CloudWatch, and PostgresAI connectors (#468)
- Supabase, Jira, and GitLab connectors (#472)
- GitHub Issues connector (#474)
- HTTP JSON plugin and script plugin for extensibility (#477)
- CloudWatch SigV4 request signing (#534)
- Supabase `fetch_alerts` implementation (#533)
- Connector trait, core types, async methods, and registry (#457, #465)
- `NormalizedMetric` and `MetricCategory` types (#480)
- Connector health status included in `--report` output (#492)
- Bidirectional issue sync manager (#486)
- Mock test infrastructure for connectors (#491)

#### Governance
- AAA architecture: Analyzer / Actor / Auditor triangle (#516, #522)
- Proposal dispatcher wired into the monitoring loop (#504, #515)
- VetoTracker and post-action verification in dispatcher (#509)
- Auto promotion eligibility tracking (#510)
- Circuit breaker and Auto-level permitted action constraints
- LLM adversarial review module (#521)
- LLM auditor wired to AI providers (#529)
- Audit log file persistence (#523)
- Audit persistence wired into dispatcher (#528)
- Post-action verification persisted in audit log (#450)
- Health check protocol schema and registry (#505)
- Supervised mode proposals across all nine analyzers (#427–#440)

#### Notifications
- PagerDuty notification channel (#458)
- Telegram bot notification channel (#466)
- Generic webhook notification channel with HMAC signing (#447, #487)
- Severity-based notification routing (#493)
- Alert deduplication (#487)

#### CLI
- `--check` flag for non-interactive health check mode (#446)
- `--report` flag for text and JSON diagnostic reports (#449)
- `--daemon` mode with all nine analyzers in monitoring loop (#454)
- `--autonomy` flag for per-feature autonomy granularity (#527)
- `--update` / `--update-check` self-update commands (#499)
- Health check CLI command handlers (#511)

#### Distribution
- Dockerfile and systemd service units (#485)
- launchd plist for macOS (#485)
- Homebrew formula (#497)
- Install script (`scripts/install.sh`) (#498)
- Helm chart for Kubernetes deployment

#### UX
- pgcli-style dropdown completion in the REPL (#542)
- SSH tunnel with `known_hosts` verification (#539)
- Bidirectional issue sync across connectors (#486)
- `/init` command to scaffold `.rpg.toml` and `POSTGRES.md` (#378)
- `\observe [duration]` command for live metric streaming (#445)
- `\autonomy` REPL command for per-feature autonomy control (#388)
- AAA governance commands in the REPL (`\dba`, `\governance`) (#516, #522)
- Health check commands wired into the REPL (#517)
- Auto-EXPLAIN mode with `\set EXPLAIN` and F5 cycling (#376)

#### Health Checks
- Health check protocol schema definition (#505)
- Connector health status registry (#492)
- CLI command handlers for health checks (#511)
- Health check commands integrated into the REPL (#517)

#### Analyzers
- Vacuum health observer (#408)
- Bloat health observer (#409)
- Query optimization observer (#412)
- Config tuning observer (#413)
- Replication health analyzer (#417)
- Connection management analyzer (#418)
- Backup monitoring analyzer (#419)
- Security analyzer (#423)
- RCA analyzer wired into `\dba rca` subcommand (#422)
- Vacuum, bloat, and config tuning analyzers in daemon mode (#439)
- All nine analyzers integrated into the monitoring loop (#454)

#### Connection & psql Compatibility
- `sslmode` support for `allow`, `verify-ca`, and `verify-full` with custom CA (#382)
- Client certificate auth via `PGSSLCERT` / `PGSSLKEY` (#389)
- `PGOPTIONS` env var and `options` conninfo key (#390)
- `pg_service.conf` support (#395)
- Conditional commands `\if` / `\elif` / `\else` / `\endif` (#396)
- Multi-host connection strings and `target_session_attrs` (#397)
- Real SSL status line showing TLS version and cipher suite (#398)
- `\copy FROM/TO PROGRAM` support (#401)
- `\crosstabview` pivot command (#402)
- Large object commands `\lo_import`, `\lo_export`, `\lo_list`, `\lo_unlink` (#403)
- Foreign data wrapper describe commands `\des`, `\dew`, `\det`, `\deu` (#407)

### Changed

- Renamed project to rpg across all source and deploy files (#453)
- Connector config unified with daemon integration (#481)
- Per-feature autonomy granularity replaces single global setting (#527)
- Refactored to explicit Tokio runtime construction (#541)
- Removed module-level `dead_code` suppressions in favour of targeted attributes (#535)

### Fixed

- REPL help text, missing `\pset` options, and variable listings (#381)

### Internal

- CI connection test suite comparing rpg vs psql output (golden file tests) (#379)
- Deploy files and scripts updated to rpg naming (#453)
- Stale infrastructure comments removed (#538)
