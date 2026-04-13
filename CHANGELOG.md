# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [0.11.0] - 2026-04-13

### Added

- **psql compatibility: 222 of 232 native PostgreSQL regression tests passing.** rpg now runs PostgreSQL's own regression test suite (unmodified `.sql` files from the postgres source tree) in CI — both psql and rpg execute the same queries against the same server, outputs are normalized and diff'd. Pass means byte-identical output. This is the most rigorous psql compatibility validation available. Full report: [`docs/psql-compat.md`](docs/psql-compat.md). Key improvements in this release:
  - 8 new describe commands: `\dP` (partitioned relations), `\dA`/`\dAc` (access methods / operator classes), `\dO` (collations), `\dF`/`\dFd`/`\dFp`/`\dFt` (text search objects) (#806)
  - `standard_conforming_strings` GUC tracking — fixes backslash parsing in `E''` strings (#808)
  - Async NOTICE/WARNING buffering for deterministic output ordering (#807)
  - `rpg:file:line:` error location prefix matching psql's `-f` behaviour (#803)
  - Wrapped format trailing-space and padding fixes (#809)
  - 3 previously skipped tests un-skipped: `copydml`, `transactions`, `plpgsql` (#810)
- **WASM browser build (experimental).** rpg compiles to `wasm32-unknown-unknown` for browser use via WebSocket proxy. SQL queries, most `\` meta-commands, `/version`, `/dba`, and error reporting all work. Line editing with arrow keys, history, and Ctrl-U/K/W supported. See [`docs/WASM.md`](docs/WASM.md). (#759)
- **Connection info + SSL status on startup.** The welcome banner now shows database, user, host, port, and TLS protocol/cipher — matching psql startup output.
- **EXPLAIN syntax highlighting.** Plain `EXPLAIN` output now gets color-coded nodes, costs, and row counts, matching the existing enhanced format. (#812, #750)
- **`/ash` sample timeout configurable.** The live `pg_stat_activity` query timeout is now a setting, avoiding freezes on heavily loaded servers. (#805, #771)

### Fixed

- **`/plan` now prepends `EXPLAIN`** to SQL queries instead of running them directly. (#811)
- **Apple Terminal status line.** Skip `DECSTBM` escape on Apple Terminal to prevent scroll-region rendering artifacts.
- **AI config hint.** When AI key is not set, the message now shows the config file path (`~/.config/rpg/config.toml`) instead of listing individual commands.
- **CodeQL cleartext-logging.** Password no longer flows through connection display or logging paths; TLS metadata queried from `pg_stat_ssl` on the server instead of the password-tainted client handshake. (#817)

### Tests

- Multi-host failover tests enabled in CI. (#757)
- PostgreSQL regress test script portable to bash 3.2. (#801)
- PG14–17 partition test output normalization. (#800)
- WAL bytes timing variance normalization. (#802)

## [0.10.2] - 2026-04-02

### Fixed

- **`/ash` cursor no longer obscures bars.** The cursor column now renders as `▐` (right half-block) with bar color preserved in the right half and a white cursor line in the left half. (#777)
- **`/ash` floating overlay moved LEFT** of the cursor with a `▶` arrow pointing to the cursor line, so the selected bucket's wait breakdown is fully visible. Uses `Clear` to prevent transparency artifacts. (#777)
- **`/ash` display freeze after Esc fixed.** Pressing Esc to exit cursor/history mode now returns to Live immediately instead of hanging for up to 60 seconds. (#777)
- **`/ash` overlay timestamp** now shows `HH:MM:SS` (was `HH:MM`). (#777)
- **`/ash` X-axis labels** right-aligned with bars and visible even with few data points; overlap prevention added. (#777)
- **`/ash` horizontal grid lines** (`┈`) added at Y-axis label rows for easier visual alignment. (#777)
- **Cursor column detection** fixed: panning beyond history no longer incorrectly highlights the leftmost column. (#777)
- **Overlay positioning** uses saturating arithmetic throughout, preventing `u16` overflow panics in debug builds on wide terminals. (#777)

## [0.10.1] - 2026-04-01

### Added

- **`/rpg` — The Haunted Cluster.** A secret PostgreSQL text adventure Easter egg. Fight the Seq Scan Ogre, solve DBA puzzles, and defeat the Absent Daemon before XID wraparound destroys the cluster. Type `/rpg` at the prompt to play.

## [0.10.0] - 2026-04-01

### Added

- **`/ash` history pan with `←`/`→`.** Left arrow pans backward in time (auto-switches Live → History mode); right arrow pans forward and returns to Live when reaching "now". Display freezes during pan so the user can inspect a specific moment. (#774, closes #773)
- **`/ash` cursor crosshair.** When panning, a bright yellow `▌` line marks the selected column. A floating overlay shows timestamp, total AAS, and a color-coded breakdown of all non-zero wait types for that bucket. (#774)
- **`/ash` context-sensitive timeline.** At the WaitEvent drill level, the stacked bar chart shows individual wait events within the selected type. At the QueryId level, it shows queries within the selected event. Each sub-dimension uses a deterministic color palette. (#774)

### Changed

- **`/ash` zoom keys reassigned to `[`/`]`.** Frees up `←`/`→` for history pan. Footer hint updated. (#774)

## [0.9.2] - 2026-03-30

### Added

- **`/ash` statement_timeout protection.** Live `pg_stat_activity` queries and pg_ash history queries now run under a 500ms `statement_timeout`. On very busy clusters where the sample query itself takes too long, the tick is skipped and a `missed: N` counter appears in the status bar. The TUI never hangs. (#769, closes #769)

### Fixed

- **`/ash` Y-axis no longer skewed by key presses.** Arrow keys, space, and other inputs previously triggered an immediate re-sample, producing extra data points per second and inflating the Y-axis. Key events are now drained within the current sample interval (wall-clock anchored); the outer loop advances to the next sample only when the full interval has elapsed. Navigation and drill-down still feel instant — the frame redraws immediately on any key press. (#773)
- **`:varname` aliases now expand before backslash dispatch.** Setting `\set dba '\i /path/start.psql'` and typing `:dba` now correctly triggers `\i` rather than being treated as SQL. (#741, closes #741)

### Changed

- **Richer connect banner.** Version string includes commit count and short hash when built past a release tag (e.g. `rpg 0.9.1+5-f7816078`). Full server version and AI provider/model shown on connect. (#766)
- **`\` vs `/` command convention documented.** `\` = psql-compatible meta-commands (unchanged muscle memory); `/` = rpg-specific extensions (AI and non-AI). README and `\?` help updated. (#766)

## [0.9.1] - 2026-03-29

### Fixed

- **`:varname` alias expansion before backslash dispatch** (#767, closes #741)

### Changed

- Version bump to 0.9.1 following v0.9.0 release.

## [0.9.0] - 2026-03-28

### Added

- **pg_ash history integration in `/ash`.** When pg_ash is installed, `/ash` fetches historical ASH data and renders it in the same TUI. Press `h` to toggle history mode. (#762)
- **Zoom window label shows actual data span**, not ring buffer capacity. (#763)
- **`\` vs `/` command convention** in README. (#764)

## [0.8.4] - 2026-03-25

### Added

- **Interactive history picker (`\s`).** Press `\s` to open a fuzzy-searchable TUI list of recent query history. Entries can be selected and inserted into the prompt, or written to a file with `\s filename`. (#701)

### Fixed

- **`sslmode=verify-ca` no longer fails with SAN-bearing certificates.** On rustls 0.23 / webpki 0.103+, servers that present certificates with Subject Alternative Names return `CertificateError::NotValidForNameContext` instead of the legacy `NotValidForName` variant. `NoCnVerifier` now catches both variants, so verify-ca correctly skips hostname validation while still verifying the certificate chain. (#738, closes #712)

### Tests

- Unit test coverage increased from 68% toward 75% (+107 tests, 1809 total). (#702)

## [0.8.3] - 2026-03-24

### Fixed

- **URI query-string `host=` and `port=` parameters now respected.** Previously, passing `postgres://ignored:9999/db?host=localhost&port=5433` would silently discard the query-string overrides and attempt to connect to `ignored:9999`. The internal URI parser has been replaced with delegation to `tokio_postgres::Config::from_str()`, which handles all standard libpq parameters correctly. (#731)
- **Default socket detection now finds any `.s.PGSQL.<port>` socket**, not only port 5432. `default_host_port()` scans well-known socket directories for any PostgreSQL socket file and returns the lowest-numbered port found, with port 5432 fast-pathed for the common case. (#728)
- **Integration tests** for the full connection path matrix (groups A–G from issue #709). (#730)

## [0.8.2] - 2026-03-24

### Fixed

- **`sslmode=prefer` now upgrades to TLS correctly.** Previously, prefer mode used certificate-verification TLS config, which caused a handshake failure when connecting to servers with self-signed or non-public-CA certificates. prefer now uses the same no-verify config as `sslmode=require`, matching psql semantics: prefer encryption, but don't require a trusted certificate. (#726)
- **`default_host()` now checks for socket file existence**, not just the socket directory. On systems where the socket directory exists but no PostgreSQL instance is running, rpg previously fell through to a misleading TCP connection attempt. (#725)

## [0.8.1] - 2026-03-24

### Fixed

- **Connection errors now show the real cause** instead of the opaque "db error" or "error connecting to server" messages. rpg walks the error source chain to surface the underlying OS/network error, e.g. "Connection refused (os error 111)" or "No such host is known" — matching psql behavior. (#708)
- **`sslmode=require` now works correctly** with self-signed certificates and non-public-CA servers. Previously, rpg verified the server certificate even in `require` mode, causing a TLS handshake failure. `sslmode=require` means encrypt only — no certificate verification — which is the correct psql semantics. SSL error messages are also improved: "SSL error: server does not support SSL" when connecting with require to a non-TLS server. (#711)

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
