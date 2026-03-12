# Project Alpha — Specification

## 1. Vision

**A single Rust binary that replaces `psql` and becomes the primary interface between humans and Postgres — with an AI brain that can observe, analyze, act, and learn.**

The world's most popular database deserves a terminal built for 2026, not 1996. Project Alpha is:

- A **psql replacement** that respects 30 years of muscle memory
- A **diagnostic powerhouse** with built-in DBA tooling
- An **AI-native terminal** where natural language and SQL coexist
- An **autonomous agent** that can manage database health at configurable autonomy levels

The end state: a DBA-in-a-box that any engineer can use, and any DBA can trust.

---

## 2. Goals

### Primary Goals

1. **psql compatibility** — a user should be able to `alias psql=alpha` and not notice for 95% of their workflow
2. **Zero-dependency deployment** — single static binary, no runtime deps, runs everywhere psql runs
3. **AI-first UX** — natural language queries, error explanation, EXPLAIN interpretation, schema-aware suggestions
4. **Autonomous operations** — configurable autonomy levels from read-only monitoring to full autopilot
5. **Connector ecosystem** — pull data from and push actions to external systems (Datadog, pganalyze, RDS, Supabase, Jira, GitHub)

### Non-Goals (for v1)

- GUI / web interface (terminal only)
- Supporting non-Postgres databases
- Replacing pg_dump / pg_restore / pg_basebackup
- Full `.psqlrc` compatibility (partial is fine)
- Mobile / embedded targets

---

## 3. Requirements

### 3.1 Functional Requirements

#### FR-1: Postgres Wire Protocol
- Connect via TCP and Unix domain sockets
- Wire protocol v3 (simple query, extended query protocol)
- Authentication: password, md5, SCRAM-SHA-256
- SSL/TLS via rustls (with native-tls fallback option)
- Connection parameters: host, port, dbname, user, password, sslmode, application_name
- Environment variables: PGHOST, PGPORT, PGDATABASE, PGUSER, PGPASSWORD, PGPASSFILE, PGSSLMODE
- `.pgpass` file support
- Connection URI format: `postgresql://user:pass@host:port/db?sslmode=require`
- libpq-compatible connection string format

#### FR-2: REPL
- Interactive readline with history (persistent across sessions)
- Multi-line input with continuation prompts
- Customizable prompts (`\set PROMPT1`, `\set PROMPT2`)
- Command history search (Ctrl-R)
- Ctrl-C cancels current query (sends CancelRequest)
- Ctrl-D exits
- `\q` exits

#### FR-3: Backslash Meta-Commands

**Tier 1 — Must have (Phase 0):**
| Command | Description |
|---------|-------------|
| `\d [pattern]` | Describe table/index/sequence/view |
| `\dt[+] [pattern]` | List tables |
| `\di[+] [pattern]` | List indexes |
| `\ds[+] [pattern]` | List sequences |
| `\dv[+] [pattern]` | List views |
| `\df[+] [pattern]` | List functions |
| `\dn[+] [pattern]` | List schemas |
| `\du [pattern]` | List roles |
| `\l[+]` | List databases |
| `\c [dbname]` | Connect to database |
| `\x [on|off|auto]` | Toggle expanded output |
| `\timing [on|off]` | Toggle query timing |
| `\q` | Quit |
| `\?` | Help |
| `\h [command]` | SQL command help |

**Tier 2 — Must have (Phase 1):**
| Command | Description |
|---------|-------------|
| `\set [name [value]]` | Set/show variables |
| `\unset name` | Unset variable |
| `\pset [option [value]]` | Set output format options |
| `\a` | Toggle aligned/unaligned |
| `\t` | Toggle tuples-only |
| `\e [file]` | Edit query in $EDITOR |
| `\i file` | Execute commands from file |
| `\o [file]` | Send output to file |
| `\copy` | Client-side COPY |
| `\watch [interval]` | Re-execute query periodically |
| `\g [file]` | Execute query (optionally to file) |
| `\gset [prefix]` | Execute and store results as variables |
| `\gexec` | Execute each result value as a statement |
| `\dp [pattern]` | List privileges |
| `\db[+]` | List tablespaces |
| `\dT[+] [pattern]` | List data types |
| `\dx[+] [pattern]` | List extensions |
| `\sf[+] function` | Show function definition |
| `\sv[+] view` | Show view definition |
| `\conninfo` | Show connection info |
| `\encoding [enc]` | Show/set encoding |
| `\password [user]` | Change password |
| `\! [command]` | Execute shell command |
| `\cd [dir]` | Change directory |
| `\echo text` | Print text |
| `\qecho text` | Print to query output |
| `\ir file` | Include file (relative) |
| `\prompt [text] name` | Prompt user for variable |

**Tier 3 — Nice to have (Phase 2+):**
| Command | Description |
|---------|-------------|
| `\lo_*` | Large object commands |
| `\crosstabview` | Pivot results |
| `\gdesc` | Describe result columns |
| `\bind` | Bind parameters |

#### FR-4: Output Formatting
- **Aligned** (default) — columns aligned with headers and borders
- **Expanded** (`\x`) — vertical format, one column per line
- **Unaligned** — separator-delimited
- **CSV** — RFC 4180 compliant
- **HTML** — table markup
- **JSON** — array of objects
- **LaTeX** — table markup
- **Wrapped** — wrap long columns
- Configurable: border style, null display, field separator, record separator, line style
- Row count footer
- Pager integration (built-in TUI pager + external pager support)

#### FR-5: Scripting & Piping
- `-c "SQL"` — execute single command
- `-f file` — execute file
- `-v name=value` — set variable
- `-X` — skip .psqlrc
- `-A` — unaligned output
- `-t` — tuples only
- `-P option=value` — set pset option
- `-o file` — output to file
- Stdin/stdout piping: `echo "SELECT 1" | alpha`
- ON_ERROR_STOP, ON_ERROR_ROLLBACK
- AUTOCOMMIT
- Exit code: 0 on success, 1 on error, 2 on connection failure

#### FR-6: Schema-Aware Autocomplete
- Table, column, schema, function, type, keyword completion
- Context-aware: after FROM → suggest tables, after SELECT → suggest columns of tables in FROM
- Refreshes on schema changes (\d commands trigger refresh)
- Supports quoted identifiers
- Completes across schemas when schema-qualified

#### FR-7: Syntax Highlighting
- SQL keywords, identifiers, strings, numbers, comments
- Real-time in input buffer
- Configurable color scheme (or auto-detect terminal theme)
- Disable-able: `--no-highlight` or `\set HIGHLIGHT off`

#### FR-8: Integrated TUI Pager
- Activates automatically when output exceeds terminal height
- Vertical and horizontal scrolling
- Column freezing (freeze leftmost N columns while scrolling right)
- Search within results (/ and ?)
- Column sorting (click or key)
- Copy cell/row/column to clipboard
- Toggle between pager and raw output
- Configurable: `\set PAGER internal` / `\set PAGER less` / `\set PAGER off`

#### FR-9: Built-in Diagnostics (`\dba`)
- `\dba activity` — pg_stat_activity with intelligent formatting
- `\dba bloat` — table and index bloat estimates
- `\dba locks` — lock tree visualization
- `\dba unused-idx` — indexes never used since last stats reset
- `\dba seq-scans` — tables with high sequential scan counts
- `\dba cache-hit` — buffer cache hit ratio by table
- `\dba vacuum` — autovacuum status, dead tuples, last vacuum times
- `\dba replication` — replication slots, lag, WAL positions
- `\dba config [param]` — non-default config with source and context
- `\dba connections` — connection counts by state, user, application
- `\dba tablesize` — table sizes including TOAST and indexes
- `\dba waits` — pg_ash wait event summary (if pg_ash available)
- All queries version-aware (adapt SQL to PG version)

#### FR-10: AI / LLM Integration
- `/ask <natural language>` — generate SQL from natural language, show it, optionally execute
- `/explain` — run EXPLAIN ANALYZE on last/given query, interpret the plan
- `/fix` — explain last error with fix suggestions
- `/optimize <query>` — suggest query rewrites and missing indexes
- `/describe <table>` — AI-generated description of table purpose and relationships
- Inline error suggestions (automatic, can be disabled)
- Schema context: table definitions, indexes, constraints, statistics fed to LLM
- Session context: recent queries and results as conversation history
- pg_ash context: wait event data when available
- Pluggable backends: OpenAI API, Anthropic API, Ollama (local), custom endpoint
- Streaming responses displayed in terminal
- Token usage tracking and budget limits
- `\set AI_PROVIDER`, `\set AI_MODEL`, `\set AI_API_KEY`
- Works without AI configured (all AI features simply unavailable, no errors)

#### FR-11: Autonomy Levels
- Configurable via `--level L1|L2|L3|L4|L5` or `\set AUTONOMY L3`
- **L1 MONITOR** — observe only, alert on issues
- **L2 ADVISE** — generate recommendations with copy-pasteable commands
- **L3 ASSIST** — auto-execute safe operations:
  - `ANALYZE` on tables with stale statistics
  - `REINDEX CONCURRENTLY` on bloated indexes
  - `VACUUM` (not FULL) on tables with dead tuples
  - `ALTER SYSTEM SET` + `pg_reload_conf()` for safe config parameters
  - `pg_cancel_backend()` for long-running queries (with configurable threshold)
- **L4 OPERATE** — L3 plus:
  - `CREATE INDEX CONCURRENTLY` for suggested indexes
  - `DROP INDEX CONCURRENTLY` for unused indexes (with grace period)
  - `VACUUM FULL` during maintenance windows
  - Connection termination (`pg_terminate_backend()`)
  - Replication slot management
- **L5 AUTOPILOT** — L4 plus:
  - `DROP TABLE`, `DROP COLUMN` for cleanup (with backup verification)
  - Major version upgrade orchestration
  - Failover decisions
  - Schema migrations
- Each action logged with: timestamp, autonomy level, action taken, justification, outcome
- Dry-run mode: show what *would* be done without doing it
- Approval workflow: L4/L5 actions can require interactive approval

#### FR-12: Connectors

**pg_ash (native):**
- Direct query of pg_ash tables
- Wait event aggregation and visualization
- Top queries by wait time
- Active session history timeline

**Datadog:**
- Pull metrics via Datadog API
- Query custom metrics, monitors, dashboards
- Correlate Datadog alerts with database events
- Auth: DD_API_KEY, DD_APP_KEY

**pganalyze:**
- Pull query statistics, EXPLAIN plans
- Index advisor suggestions
- Auth: PGANALYZE_API_KEY

**AWS RDS:**
- Performance Insights API
- Enhanced Monitoring
- CloudWatch metrics
- Auth: AWS credentials (standard chain)

**Supabase:**
- Management API (project info, settings)
- Connect via pooler
- Auth: SUPABASE_ACCESS_TOKEN

**Issue Trackers:**
- GitHub Issues: create/update issues with RCA
- GitLab Issues: same
- Jira: create/update tickets
- Template-based issue content

#### FR-13: Operating Modes

**Interactive mode (default):**
- Human at the terminal
- AI assists in real-time
- Agent suggestions appear inline
- Full REPL experience

**Daemon mode:**
- `alpha daemon --config config.toml`
- Runs headless, no REPL
- Continuous monitoring loop
- Reports via configured channels (Slack webhook, email, GitHub issues)
- PID file, systemd unit support
- Health check endpoint (HTTP)
- Graceful shutdown on SIGTERM

**Single-shot mode:**
- `-c` / `-f` for scripting (psql-compatible)
- `--check` for health check (exit code = severity)
- `--report` for full diagnostic report to stdout/file

### 3.2 Non-Functional Requirements

#### NFR-1: Performance
- Startup time: < 100ms to first prompt (without AI init)
- Query result rendering: handle 1M+ rows without OOM (streaming)
- Memory: < 50MB baseline, < 200MB with schema cache for large databases (1000+ tables)
- Binary size: < 30MB (static, stripped)

#### NFR-2: Portability
- Linux x86_64, aarch64 (primary targets)
- macOS x86_64, aarch64 (primary targets)
- Windows x86_64 (secondary, best effort)
- Static linking preferred (musl on Linux)
- No runtime dependencies

#### NFR-3: Security
- No credentials stored in plaintext by the tool itself
- Respect .pgpass, PGPASSWORD, connection URIs
- AI API keys: environment variables or config file with 600 permissions
- Autonomy actions: logged, auditable, reversible where possible
- Daemon mode: drop privileges, chroot-able
- No telemetry without explicit opt-in

#### NFR-4: Compatibility
- Postgres 12-18 (and upcoming versions)
- Forward-compatible: gracefully degrade on unknown PG versions
- pgBouncer / PgCat / Supavisor connection pooler compatible
- Works through SSH tunnels and port forwarding

---

## 4. Architectural Choices

### 4.1 Language: Rust

**Why Rust:**
- Single static binary (no Python/Ruby/Node runtime)
- Predictable performance, no GC pauses
- Memory safety without runtime overhead
- Excellent async ecosystem (tokio)
- Cross-compilation story is mature
- Growing Postgres ecosystem (pgx/pgrx community)

**Why not C (like psql):** Memory safety, dependency management, async complexity.
**Why not Go:** Less control over memory layout, larger binaries, GC pauses during large result rendering.
**Why not Python (like pgcli):** Startup time, distribution pain, performance ceiling.

### 4.2 Async Runtime: Tokio

- Industry standard for async Rust
- `tokio-postgres` is the most mature async PG driver
- Needed for: concurrent query cancellation, daemon mode, connector HTTP calls, streaming
- Single-threaded runtime sufficient initially; multi-threaded for daemon mode

### 4.3 Wire Protocol: tokio-postgres (with caveats)

Use `tokio-postgres` for the wire protocol layer, but **wrap it** — we need control over:
- Raw protocol messages for `\copy` (COPY protocol)
- CancelRequest sending
- Notice and notification handling
- Connection parameter negotiation
- Future: logical replication protocol

Strategy: start with `tokio-postgres`, extract/fork the protocol layer when needed.

Alternative considered: raw implementation using `bytes` + `tokio::net`. Too much work initially, but may be the end state for full control.

### 4.4 REPL: rustyline

- Most mature Rust readline implementation
- History, completion, hints, key bindings
- Custom `Completer`, `Highlighter`, `Hinter`, `Validator` traits
- Limitation: not async-native (blocks on input). Workaround: run in dedicated thread, communicate via channels.

### 4.5 TUI Pager: ratatui + crossterm

- `ratatui` is the standard Rust TUI framework
- `crossterm` for cross-platform terminal manipulation
- Pager is a separate mode: enters when output exceeds terminal, exits on `q`
- Must coexist with readline (switch between REPL mode and pager mode)

### 4.6 AI Integration: HTTP Client + Streaming

- `reqwest` for HTTP calls to LLM APIs
- Server-Sent Events (SSE) for streaming responses
- Abstract `LlmProvider` trait:
  ```rust
  trait LlmProvider: Send + Sync {
      async fn complete(&self, messages: &[Message], options: &CompletionOptions) -> Result<CompletionStream>;
      fn name(&self) -> &str;
      fn default_model(&self) -> &str;
  }
  ```
- Implementations: OpenAI, Anthropic, Ollama
- Schema serialization: compact DDL format (not full pg_dump) to minimize tokens
- Context budget: allocate % of context window to schema, history, pg_ash data

### 4.7 Configuration

**Hierarchy (lowest to highest priority):**
1. Compiled defaults
2. `/etc/alpha/config.toml` (system)
3. `~/.config/alpha/config.toml` (user)
4. `ALPHA_*` environment variables
5. Command-line flags
6. `\set` commands (session only)

**Format:** TOML

```toml
[connection]
default_host = "localhost"
default_port = 5432
default_sslmode = "prefer"

[display]
pager = "internal"          # internal | external | off
theme = "auto"              # auto | dark | light | none
null_display = "∅"
border_style = "unicode"    # ascii | unicode | none
expanded = "auto"           # on | off | auto

[ai]
provider = "anthropic"      # openai | anthropic | ollama | custom
model = "claude-sonnet-4-20250514"
api_key_env = "ANTHROPIC_API_KEY"
auto_explain_errors = true
max_tokens_per_request = 4096
monthly_budget_usd = 50.0

[agent]
autonomy_level = "L2"
check_interval_seconds = 60
maintenance_window = "02:00-06:00 UTC"

[connectors.datadog]
enabled = false
api_key_env = "DD_API_KEY"
app_key_env = "DD_APP_KEY"
site = "datadoghq.com"

[connectors.github]
enabled = false
token_env = "GITHUB_TOKEN"
default_repo = ""

[logging]
level = "info"
file = "~/.local/share/alpha/alpha.log"
action_log = "~/.local/share/alpha/actions.log"
```

### 4.8 Project Structure

```
project-alpha/
├── Cargo.toml
├── Cargo.lock
├── src/
│   ├── main.rs                 # Entry point, CLI parsing
│   ├── repl/
│   │   ├── mod.rs              # REPL loop
│   │   ├── readline.rs         # rustyline integration
│   │   ├── completer.rs        # Schema-aware autocomplete
│   │   ├── highlighter.rs      # Syntax highlighting
│   │   └── history.rs          # History management
│   ├── protocol/
│   │   ├── mod.rs              # Wire protocol abstraction
│   │   ├── connection.rs       # Connection management
│   │   ├── query.rs            # Query execution
│   │   ├── copy.rs             # COPY protocol
│   │   └── cancel.rs           # Query cancellation
│   ├── commands/
│   │   ├── mod.rs              # Command dispatcher
│   │   ├── parser.rs           # Backslash command parser
│   │   ├── describe.rs         # \d family
│   │   ├── list.rs             # \l, \dt, \di, etc.
│   │   ├── settings.rs         # \set, \unset, \pset
│   │   ├── io.rs               # \i, \o, \e, \copy
│   │   ├── dba.rs              # \dba family
│   │   └── help.rs             # \?, \h
│   ├── display/
│   │   ├── mod.rs              # Output formatting dispatcher
│   │   ├── aligned.rs          # Aligned table format
│   │   ├── expanded.rs         # Expanded (\x) format
│   │   ├── csv.rs              # CSV format
│   │   ├── json.rs             # JSON format
│   │   └── pager.rs            # TUI pager (ratatui)
│   ├── ai/
│   │   ├── mod.rs              # AI subsystem
│   │   ├── provider.rs         # LlmProvider trait
│   │   ├── openai.rs           # OpenAI implementation
│   │   ├── anthropic.rs        # Anthropic implementation
│   │   ├── ollama.rs           # Ollama implementation
│   │   ├── context.rs          # Schema/session context builder
│   │   └── commands.rs         # /ask, /explain, /fix, /optimize
│   ├── agent/
│   │   ├── mod.rs              # Agent subsystem
│   │   ├── autonomy.rs         # Autonomy level definitions
│   │   ├── monitor.rs          # Health monitoring loop
│   │   ├── actions.rs          # Remediation actions
│   │   ├── protocols.rs        # Health check protocol engine
│   │   └── action_log.rs       # Action audit log
│   ├── connectors/
│   │   ├── mod.rs              # Connector trait and registry
│   │   ├── pg_ash.rs           # pg_ash integration
│   │   ├── datadog.rs          # Datadog API
│   │   ├── pganalyze.rs        # pganalyze API
│   │   ├── rds.rs              # AWS RDS APIs
│   │   ├── supabase.rs         # Supabase API
│   │   ├── github.rs           # GitHub Issues
│   │   ├── gitlab.rs           # GitLab Issues
│   │   └── jira.rs             # Jira API
│   ├── daemon/
│   │   ├── mod.rs              # Daemon mode
│   │   ├── scheduler.rs        # Check scheduling
│   │   ├── notify.rs           # Alert channels (Slack, email)
│   │   └── health.rs           # HTTP health endpoint
│   ├── config/
│   │   ├── mod.rs              # Config loading and merging
│   │   └── schema.rs           # Config struct definitions
│   └── util/
│       ├── mod.rs
│       ├── pg_version.rs       # PG version detection and compat
│       └── format.rs           # Shared formatting utilities
├── tests/
│   ├── integration/
│   │   ├── connect.rs          # Connection tests
│   │   ├── commands.rs         # Meta-command tests
│   │   └── output.rs           # Output format tests
│   └── fixtures/
│       └── ...                 # Test schemas, expected outputs
├── docs/
│   ├── commands.md             # Full command reference
│   ├── ai.md                   # AI feature documentation
│   └── agent.md                # Agent/autonomy documentation
└── scripts/
    ├── build-release.sh        # Cross-compilation builds
    └── test-compat.sh          # psql compatibility test suite
```

---

## 5. Implementation Plan

### Phase 0: Bootstrap (Weeks 1-4)

**Goal:** Connect to Postgres, run queries, display results. The absolute minimum to be useful.

**Week 1-2:**
- [ ] Project scaffold: Cargo.toml, CI (GitHub Actions), cross-compilation targets
- [ ] Connection: parse connection params (URI, env vars, flags), connect via `tokio-postgres`
- [ ] Simple query execution: send SQL, receive RowDescription + DataRow, display
- [ ] Basic aligned output formatting with headers
- [ ] Basic REPL: rustyline loop, history file, multi-line input detection (semicolons)

**Week 3-4:**
- [ ] `\d`, `\dt`, `\di`, `\l`, `\c` — the essentials
- [ ] `\x` expanded output
- [ ] `\timing`
- [ ] `\q` and Ctrl-D exit
- [ ] Query cancellation (Ctrl-C → CancelRequest)
- [ ] Error display with SQLSTATE
- [ ] `-c` and `-f` flags for scripting
- [ ] `.pgpass` support

**Milestone:** Can connect to any Postgres, run queries, see results. Usable as a basic psql.

### Phase 1: Daily Driver (Weeks 5-10)

**Goal:** Good enough to be someone's default Postgres terminal.

**Week 5-6:**
- [ ] Schema-aware autocomplete (tables, columns, keywords)
- [ ] Syntax highlighting in input
- [ ] `\set`, `\unset`, `\pset`, variables
- [ ] `\e` (edit in $EDITOR)
- [ ] `\i`, `\ir` (include files)
- [ ] `\o` (output to file)

**Week 7-8:**
- [ ] `\copy` (client-side COPY TO/FROM)
- [ ] `\watch` (periodic re-execution)
- [ ] `\g`, `\gset`, `\gexec`
- [ ] CSV, JSON output formats
- [ ] `\sf`, `\sv` (show function/view source)
- [ ] `\dp`, `\db`, `\dT`, `\dx`

**Week 9-10:**
- [ ] TUI pager (ratatui): vertical/horizontal scroll, search, column freeze
- [ ] `\dba` commands: activity, bloat, locks, unused-idx, vacuum, replication
- [ ] PG version detection and query adaptation
- [ ] `.psqlrc` basic support (execute commands on startup)
- [ ] Config file loading (TOML)

**Milestone:** Can replace psql for daily use. Has autocomplete, highlighting, pager, diagnostics.

### Phase 2: AI Brain (Weeks 11-16)

**Goal:** LLM integration that makes the terminal dramatically more powerful.

**Week 11-12:**
- [ ] `LlmProvider` trait and OpenAI/Anthropic implementations
- [ ] Schema context builder (compact DDL from pg_catalog)
- [ ] `/ask` command: NL → SQL generation with streaming display
- [ ] `/fix` command: explain last error with suggestions

**Week 13-14:**
- [ ] `/explain` command: run EXPLAIN ANALYZE, feed plan to LLM, display interpretation
- [ ] `/optimize` command: suggest query rewrites and indexes
- [ ] Session context: feed recent query history to LLM
- [ ] Token tracking and budget enforcement

**Week 15-16:**
- [ ] Ollama (local model) support
- [ ] Inline error suggestions (automatic, toggle-able)
- [ ] pg_ash integration: wait event data as LLM context
- [ ] `/describe` command: AI-generated table/schema descriptions
- [ ] `\dba waits` command for pg_ash visualization

**Milestone:** AI features work end-to-end. Can ask questions in English, get SQL back, explain errors, interpret EXPLAIN plans.

### Phase 3: Agent (Weeks 17-24)

**Goal:** Autonomous monitoring and remediation with safety controls.

**Week 17-18:**
- [ ] Autonomy level framework (L1-L5 with action classification)
- [ ] Action audit log (every agent action recorded with justification)
- [ ] Monitor loop: periodic health checks in interactive and daemon mode
- [ ] L1 implementation: alert on issues (bloat, long queries, replication lag, connection saturation)

**Week 19-20:**
- [ ] L2 implementation: generate recommendations with copy-pasteable commands
- [ ] L3 implementation: auto-execute safe operations (ANALYZE, REINDEX CONCURRENTLY, VACUUM)
- [ ] Dry-run mode for all actions
- [ ] Health check protocol engine (pluggable check definitions)

**Week 21-22:**
- [ ] Daemon mode: headless operation, PID file, signal handling
- [ ] Notification channels: Slack webhook, email (SMTP)
- [ ] GitHub Issues connector: create issues with RCA from agent findings
- [ ] HTTP health check endpoint for daemon mode

**Week 23-24:**
- [ ] L4 implementation: CREATE/DROP INDEX CONCURRENTLY, VACUUM FULL, pg_terminate_backend
- [ ] Approval workflow: interactive confirmation for high-risk actions
- [ ] Maintenance window awareness
- [ ] Systemd unit file and install guide

**Milestone:** Agent can monitor a database, detect issues, and take appropriate action within configured autonomy level.

### Phase 4: Ecosystem (Weeks 25+)

**Goal:** Connect to the outside world.

- [ ] Datadog connector
- [ ] pganalyze connector
- [ ] AWS RDS connector (Performance Insights, CloudWatch)
- [ ] Supabase connector
- [ ] Jira connector
- [ ] GitLab Issues connector
- [ ] Plugin system for custom connectors
- [ ] Container image (Alpine-based, ~15MB)
- [ ] Helm chart for Kubernetes sidecar deployment
- [ ] Protocol marketplace (shareable health check definitions)
- [ ] L5 implementation (with extensive testing and safeguards)

---

## 6. Testing Strategy

### Unit Tests
- Output formatting (golden file tests: input rows → expected string output)
- Command parsing (backslash command tokenization)
- Config loading and merging
- Autonomy level action classification

### Integration Tests
- Require a running Postgres instance (Docker in CI)
- Connection with all auth methods
- All `\d` family commands against known schemas
- `\copy` round-trip
- Query cancellation
- PG version matrix: 12, 13, 14, 15, 16, 17, 18

### Compatibility Tests
- Run the same commands in psql and Project Alpha, diff the output
- Scripted test suite: `test-compat.sh` runs `-c` commands in both and compares
- Target: < 5% divergence in output formatting for common commands

### AI Tests
- Mock LLM responses for deterministic testing
- Schema context builder: verify compact DDL generation
- Token budget enforcement

### Agent Tests
- Simulated databases with known issues (bloated indexes, stale stats, long queries)
- Verify correct action at each autonomy level
- Verify actions are logged correctly
- Verify dry-run produces no side effects

---

## 7. Distribution

### Binary Releases
- GitHub Releases with pre-built binaries:
  - `alpha-linux-x86_64` (static, musl)
  - `alpha-linux-aarch64` (static, musl)
  - `alpha-darwin-x86_64`
  - `alpha-darwin-aarch64`
  - `alpha-windows-x86_64.exe`
- Checksums (SHA256) and signatures

### Package Managers
- `brew install alpha` (Homebrew tap)
- `cargo install alpha` (crates.io)
- `.deb` and `.rpm` packages (Phase 4)
- Docker: `ghcr.io/nikolays/alpha:latest`

### Install Script
```bash
curl -sL https://get.project-alpha.dev | sh
```

---

## 8. Open Questions

1. **Name:** "Project Alpha" is the codename. Final shipping name TBD.
2. **License:** Source-available? Dual license (AGPL + commercial)? Apache 2.0? Decision impacts adoption and business model.
3. **Wire protocol:** Fork `tokio-postgres` or build from scratch? Start with tokio-postgres, evaluate after Phase 0.
4. **pgBouncer transaction mode:** How to handle features that require session-level state (prepared statements, temp tables) through poolers?
5. **Offline AI:** Should we bundle a small local model (e.g., quantized Phi-3) for environments without internet? Or is Ollama sufficient?
6. **Multi-database:** Should daemon mode monitor multiple databases from one process, or one process per database?
7. **Plugin API stability:** When do we commit to a stable plugin interface for custom connectors?

---

*This is a living document. Update as decisions are made and requirements evolve.*
