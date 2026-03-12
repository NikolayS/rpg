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

**AWS CloudWatch:**
- CloudWatch metrics (CPU, memory, IOPS, network, disk)
- CloudWatch Logs (Postgres logs via log_destination)
- CloudWatch Alarms (status, history)
- RDS Performance Insights API (wait events, top SQL, load)
- RDS Enhanced Monitoring (OS-level metrics)
- RDS Events (maintenance, failover, configuration changes)
- Auth: AWS credentials (standard chain — env vars, ~/.aws/credentials, IAM role)

**Supabase:**
- Management API (project info, settings)
- Connect via pooler
- Auth: SUPABASE_ACCESS_TOKEN

**PostgresAI Monitoring & Checkup:**
- Pull monitoring data from PostgresAI platform
- Historical metrics, query performance, health scores
- Checkup reports: automated health assessments with recommendations
- Compare current state against PostgresAI baselines and best practices
- Auth: POSTGRESAI_API_KEY + project/org identifiers

**PostgresAI Issues:**
- Read/create/update issues in PostgresAI's issue tracking system
- Link RCA findings to existing issues
- Auto-create issues from agent-detected problems
- Track remediation status (open → in progress → resolved → verified)
- Attach evidence: query plans, wait event snapshots, metric graphs
- Bidirectional sync with external trackers (GitHub, Jira) when configured
- Auth: same POSTGRESAI_API_KEY

**Issue Trackers (external):**
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
- **Linux x86_64** — primary, static (musl)
- **Linux aarch64** — primary, static (musl)
- **macOS x86_64** — primary
- **macOS aarch64 (Apple Silicon)** — primary
- **Windows x86_64** — primary
- **Windows aarch64** — primary
- All six targets are first-class, tested in CI, included in every release
- Static linking on Linux (musl), dynamic on macOS/Windows (system TLS)
- No runtime dependencies beyond OS-provided libraries

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

### Phase 0: psql Replacement (Weeks 1-8)

**Goal:** A drop-in psql replacement. No AI, no agent, no extras — just a Rust binary that does everything psql does. If a user can't `alias psql=alpha` and keep working, this phase isn't done.

**Week 1-2: Connect and Query**
- [ ] Project scaffold: Cargo.toml, CI (GitHub Actions)
- [ ] Cross-compilation: Linux x86_64/aarch64 (musl), macOS x86_64/aarch64, Windows x86_64/aarch64
- [ ] Connection: parse all connection params (host, port, dbname, user, password, sslmode, application_name)
- [ ] Connection URI format: `postgresql://user:pass@host:port/db?options`
- [ ] libpq-style connection strings
- [ ] Environment variables: PGHOST, PGPORT, PGDATABASE, PGUSER, PGPASSWORD, PGSSLMODE, PGCONNECT_TIMEOUT
- [ ] `.pgpass` file support
- [ ] SSL/TLS (rustls + native-tls fallback)
- [ ] Auth: password, md5, SCRAM-SHA-256
- [ ] Unix domain sockets (Linux/macOS)
- [ ] Wire protocol v3: simple query, extended query protocol
- [ ] Basic REPL: rustyline loop, persistent history file, multi-line input (semicolons)
- [ ] Query execution with result display (aligned format with headers)
- [ ] Row count footer ("(N rows)")
- [ ] Query timing display
- [ ] Error display with SQLSTATE, detail, hint, context, position

**Week 3-4: Core Meta-Commands**
- [ ] `\d [pattern]` — describe table/index/sequence/view (match psql output exactly)
- [ ] `\dt[+]`, `\di[+]`, `\ds[+]`, `\dv[+]`, `\dm[+]` — list tables/indexes/sequences/views/materialized views
- [ ] `\df[+] [pattern]` — list functions
- [ ] `\dn[+]` — list schemas
- [ ] `\du`, `\dg` — list roles
- [ ] `\dp` — list privileges (ACLs)
- [ ] `\db[+]` — list tablespaces
- [ ] `\dT[+]` — list data types
- [ ] `\dx[+]` — list extensions
- [ ] `\dE[+]` — list foreign tables
- [ ] `\dD[+]` — list domains
- [ ] `\dc[+]` — list conversions
- [ ] `\dC[+]` — list casts
- [ ] `\dd` — show object descriptions
- [ ] `\des[+]` — list foreign servers
- [ ] `\dew[+]` — list foreign-data wrappers
- [ ] `\det[+]` — list foreign tables
- [ ] `\deu[+]` — list user mappings
- [ ] `\l[+]` — list databases
- [ ] `\c [dbname [user] [host] [port]]` — connect to database
- [ ] `\conninfo` — show current connection info
- [ ] `\x [on|off|auto]` — toggle expanded output
- [ ] `\timing [on|off]` — toggle query timing
- [ ] `\q` — quit
- [ ] `\?` — help for backslash commands
- [ ] `\h [command]` — SQL command syntax help
- [ ] Pattern matching (wildcards) for all `\d` commands
- [ ] `+` modifier (extra detail) for all `\d` commands that support it
- [ ] `S` modifier (show system objects) where applicable

**Week 5-6: Variables, I/O, Editing**
- [ ] `\set [name [value]]` — set/show psql variables
- [ ] `\unset name` — unset variable
- [ ] Built-in variables: AUTOCOMMIT, ON_ERROR_STOP, ON_ERROR_ROLLBACK, QUIET, SINGLELINE, SINGLESTEP
- [ ] `\pset [option [value]]` — set output format options (border, format, null, fieldsep, recordsep, title, etc.)
- [ ] `\a` — toggle aligned/unaligned
- [ ] `\t [on|off]` — toggle tuples-only
- [ ] `\f [sep]` — set field separator
- [ ] `\H` — toggle HTML output
- [ ] `\e [file] [line]` — edit query/file in $EDITOR
- [ ] `\i file` — execute commands from file
- [ ] `\ir file` — include file (relative path)
- [ ] `\o [file|command]` — send output to file or pipe
- [ ] `\w file` — write query buffer to file
- [ ] `\r` — reset query buffer
- [ ] `\p` — print current query buffer
- [ ] `\echo text` — print text to stdout
- [ ] `\qecho text` — print text to query output channel
- [ ] `\warn text` — print text to stderr
- [ ] `\prompt [text] name` — prompt user for variable value
- [ ] `\! [command]` — execute shell command
- [ ] `\cd [dir]` — change directory
- [ ] `\encoding [enc]` — show/set client encoding
- [ ] `\password [user]` — change password (interactively)

**Week 7-8: COPY, Execution Variants, Scripting, Output Formats**
- [ ] `\copy ... FROM/TO` — client-side COPY with all format options (CSV, TEXT, BINARY, DELIMITER, HEADER, etc.)
- [ ] `\watch [interval]` — re-execute query periodically
- [ ] `\g [file]` — execute query, optionally send to file
- [ ] `\gset [prefix]` — execute query, store results as variables
- [ ] `\gexec` — execute each result value as a SQL statement
- [ ] `\gdesc` — describe result columns without executing
- [ ] `\sf[+] function` — show function source
- [ ] `\sv[+] view` — show view definition
- [ ] `\bind values` — set bind parameters for next query
- [ ] Output formats: aligned (default), unaligned, wrapped, CSV, HTML, LaTeX, JSON, asciidoc
- [ ] Customizable null display, border, line style, unicode/ascii
- [ ] `-c "SQL"` — execute single command and exit
- [ ] `-f file` — execute file and exit
- [ ] `-v name=value` — set variable from command line
- [ ] `-X` — skip .psqlrc
- [ ] `-A` — unaligned output
- [ ] `-t` — tuples only
- [ ] `-P option=value` — set pset from command line
- [ ] `-o file` — output to file
- [ ] `-1` / `--single-transaction` — wrap `-f` in BEGIN/COMMIT
- [ ] `-b` / `--echo-errors` — echo failed commands
- [ ] `-e` / `--echo-queries` — echo all queries sent
- [ ] `-E` / `--echo-hidden` — show queries generated by `\d` commands
- [ ] `-n` / `--no-readline` — disable readline
- [ ] `-w` / `--no-password` — never prompt for password
- [ ] `-W` / `--password` — force password prompt
- [ ] Stdin/stdout piping: `echo "SELECT 1" | alpha`
- [ ] Exit codes: 0 success, 1 error, 2 connection failure, 3 script error (match psql)
- [ ] `.psqlrc` execution on startup (skip with `-X`)
- [ ] PSQLRC environment variable
- [ ] Tab completion: SQL keywords, schema objects, file paths (for `\i`, `\copy`)
- [ ] Query cancellation: Ctrl-C sends CancelRequest to server
- [ ] Ctrl-D on empty line exits
- [ ] Customizable prompts (PROMPT1, PROMPT2, PROMPT3)
- [ ] Notification (LISTEN/NOTIFY) display
- [ ] Transaction status in prompt (\* for in-transaction, ! for failed transaction)

**Milestone:** Full psql replacement. Can `alias psql=alpha`. All common commands work. Builds and runs on all 6 platform targets. No AI, no extras — just psql in Rust.

### Phase 1: Beyond psql (Weeks 9-14)

**Goal:** Everything psql can't do. This is where we start being *better* than psql.

**Week 9-10:**
- [ ] Schema-aware contextual autocomplete (after FROM → tables, after SELECT → columns)
- [ ] Syntax highlighting in input line (SQL keywords, strings, numbers, comments)
- [ ] Configurable color themes (auto-detect dark/light terminal)
- [ ] TUI pager (ratatui): replaces external pager, vertical/horizontal scroll, search
- [ ] Pager: column freezing, column sorting, cell/row copy

**Week 11-12:**
- [ ] `\dba` diagnostic commands: activity, bloat, locks, unused-idx, seq-scans, cache-hit, vacuum, replication, connections, tablesize, config
- [ ] PG version detection — adapt diagnostic queries to server version (PG 12-18)
- [ ] Connection pooler detection (pgBouncer/PgCat/Supavisor) — warn about unsupported features
- [ ] Config file loading (TOML) — user preferences, defaults, themes

**Week 13-14:**
- [ ] `\dba waits` — pg_ash wait event summary (when pg_ash is available)
- [ ] Query history search improvements (fuzzy search, filter by table/command type)
- [ ] Bookmarks: save and recall named queries (`\bookmark save name`, `\bookmark run name`)
- [ ] `\diff` — compare table structure across two databases/schemas
- [ ] Named connections: save connection profiles in config, switch with `\c @profile-name`
- [ ] Output: sparklines and inline bar charts for numeric columns (optional)

**Milestone:** Clearly better than psql for daily use. Has autocomplete, highlighting, integrated pager, diagnostics, and quality-of-life features psql never had.

### Phase 2: AI Brain (Weeks 15-22)

**Goal:** LLM integration that makes the terminal dramatically more powerful.

**Week 15-16:**
- [ ] `LlmProvider` trait and OpenAI/Anthropic implementations
- [ ] Schema context builder (compact DDL from pg_catalog)
- [ ] `/ask` command: NL → SQL generation with streaming display
- [ ] `/fix` command: explain last error with suggestions

**Week 17-18:**
- [ ] `/explain` command: run EXPLAIN ANALYZE, feed plan to LLM, display interpretation
- [ ] `/optimize` command: suggest query rewrites and indexes
- [ ] Session context: feed recent query history to LLM
- [ ] Token tracking and budget enforcement

**Week 19-20:**
- [ ] Ollama (local model) support
- [ ] Inline error suggestions (automatic, toggle-able)
- [ ] pg_ash context: wait event data fed to LLM for deeper analysis
- [ ] `/describe` command: AI-generated table/schema descriptions
- [ ] AI mode (`\ai`) and SQL mode (`\sql`) switching

**Week 21-22:**
- [ ] Plan mode (`\plan`): AI investigates read-only, produces plan document
- [ ] YOLO mode (`\yolo`): AI auto-executes within autonomy level
- [ ] Observe mode (`\observe`): pure read-only watching with summaries
- [ ] Mode-aware prompts showing current state

**Milestone:** AI features work end-to-end. All interaction modes functional. Can ask questions in English, get SQL back, explain errors, interpret EXPLAIN plans, generate and execute plans.

### Phase 3: Agent (Weeks 23-32)

**Goal:** Autonomous monitoring and remediation with safety controls.

**Week 23-24:**
- [ ] Autonomy level framework (L1-L5 with action classification)
- [ ] Action audit log (every agent action recorded with justification)
- [ ] Monitor loop: periodic health checks in interactive and daemon mode
- [ ] L1 implementation: alert on issues (bloat, long queries, replication lag, connection saturation)

**Week 25-26:**
- [ ] L2 implementation: generate recommendations with copy-pasteable commands
- [ ] L3 implementation: auto-execute safe operations (ANALYZE, REINDEX CONCURRENTLY, VACUUM)
- [ ] Dry-run mode for all actions
- [ ] Health check protocol engine (pluggable check definitions)

**Week 27-28:**
- [ ] Daemon mode: headless operation, PID file, signal handling
- [ ] Notification channels: Slack webhook, email (SMTP)
- [ ] PostgresAI Issues connector: create/update issues with RCA from agent findings
- [ ] PostgresAI Monitoring & Checkup connector: pull baselines, health scores
- [ ] HTTP health check endpoint for daemon mode

**Week 29-30:**
- [ ] GitHub Issues connector
- [ ] L4 implementation: CREATE/DROP INDEX CONCURRENTLY, VACUUM FULL, pg_terminate_backend
- [ ] Approval workflow: interactive confirmation for high-risk actions
- [ ] Maintenance window awareness

**Week 31-32:**
- [ ] Systemd unit file and install guide
- [ ] Launchd plist for macOS
- [ ] Windows service support
- [ ] Container image (Alpine-based, ~15MB)

**Milestone:** Agent can monitor a database, detect issues, and take appropriate action within configured autonomy level. Runs as a daemon on all platforms.

### Phase 4: Ecosystem (Weeks 33+)

**Goal:** Connect to the outside world.

- [ ] Datadog connector
- [ ] pganalyze connector
- [ ] AWS CloudWatch connector (metrics, logs, alarms, RDS Performance Insights, Enhanced Monitoring)
- [ ] Supabase connector
- [ ] Jira connector
- [ ] GitLab Issues connector
- [ ] Plugin system for custom connectors
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
- GitHub Releases with pre-built binaries for all 6 targets:
  - `alpha-linux-x86_64` (static, musl)
  - `alpha-linux-aarch64` (static, musl)
  - `alpha-darwin-x86_64`
  - `alpha-darwin-aarch64`
  - `alpha-windows-x86_64.exe`
  - `alpha-windows-aarch64.exe`
- Checksums (SHA256) and signatures
- All targets built and tested in CI from Phase 0

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

## 8. Interaction Modes

Inspired by Claude Code's mode system (plan mode, YOLO mode) but adapted for the Postgres domain. Modes control **what the input means**, **what the AI can do**, and **how much autonomy the agent has**.

### 8.1 Input Modes

The terminal has two fundamental input modes, switchable with a single keystroke or command:

#### SQL Mode (default)

The classic psql experience. Input is treated as SQL or backslash commands.

```
alpha=> SELECT * FROM users WHERE id = 42;
alpha=> \dt public.*
alpha=> \dba bloat
```

- Default prompt: `dbname=>`
- Backslash commands work
- Multi-line SQL with continuation prompt
- Tab completes schema objects and keywords
- This is what psql users expect

#### AI Mode

Input is treated as natural language. The AI interprets intent and generates SQL, runs diagnostics, or takes action.

```
alpha ai> show me the 10 biggest tables
-- Generating SQL...
SELECT schemaname, tablename, 
       pg_total_relation_size(schemaname || '.' || tablename) AS total_size
FROM pg_tables 
ORDER BY pg_total_relation_size(schemaname || '.' || tablename) DESC 
LIMIT 10;
-- Run this query? [Y/n/edit]

alpha ai> why is this query slow: SELECT * FROM orders WHERE created_at > now() - interval '1 day'
-- Analyzing...
-- The orders table has 12M rows but no index on created_at.
-- Currently doing a sequential scan (cost: 847291).
-- Recommendation: CREATE INDEX CONCURRENTLY idx_orders_created_at ON orders(created_at);
-- Create this index? [Y/n] (autonomy: L3+ required)
```

- Prompt changes: `dbname ai>`
- Everything is interpreted as natural language
- AI generates SQL, shows it, asks before executing (unless in YOLO mode)
- Can still run raw SQL by prefixing with `\sql` or `;`
- Tab completes common intents: "show me...", "why is...", "fix...", "optimize..."

#### Switching Modes

```
-- From SQL mode:
\ai                     -- switch to AI mode
-- or just prefix a single query:
/ask show me table sizes -- one-shot AI, stays in SQL mode

-- From AI mode:
\sql                    -- switch back to SQL mode
-- or prefix raw SQL:
;SELECT 1               -- one-shot SQL, stays in AI mode

-- Toggle:
Ctrl-T                  -- toggle between SQL and AI mode (like Ctrl-X in nano)
```

### 8.2 Execution Modes

Orthogonal to input mode — these control *how much the AI can do without asking*.

#### Interactive (default)

The AI always shows what it wants to do and asks for confirmation before executing anything that changes state.

```
alpha ai> add an index on users.email
-- I'd like to run:
--   CREATE INDEX CONCURRENTLY idx_users_email ON users(email);
-- This is a safe operation (CONCURRENTLY, no locks).
-- Execute? [Y/n/edit]
```

- Generated SQL is shown before execution
- Read-only queries can auto-execute (configurable)
- Write/DDL requires confirmation
- User can edit the generated SQL before running

#### Plan Mode

The AI analyzes, plans, but does NOT execute. Produces a plan document.

```
alpha plan> our API is timing out on the /orders endpoint, database seems slow

-- Entering plan mode. I'll investigate and produce a plan.
-- [1/5] Checking pg_stat_activity for long-running queries...
-- [2/5] Analyzing pg_ash wait events for the last hour...
-- [3/5] Checking index usage on orders-related tables...
-- [4/5] Reviewing autovacuum status...
-- [5/5] Checking connection pool saturation...

📋 PLAN: orders endpoint performance fix
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

Root Cause: Missing index on orders.customer_id causes nested loop
with sequential scan during JOIN. Exacerbated by table bloat (34%)
and stale statistics (last ANALYZE: 3 days ago).

Actions (in order):
1. ANALYZE orders;                                    [safe, immediate]
2. CREATE INDEX CONCURRENTLY idx_orders_customer_id   [safe, ~2 min]
   ON orders(customer_id);
3. VACUUM orders;                                     [safe, ~5 min]
4. Review: connection pool size (currently 20,         [manual]
   recommend 50 based on active sessions)

Execute this plan? [Y/n/edit/save]
-- Saved to: ~/.local/share/alpha/plans/2026-03-12-orders-perf.md
```

- AI can run read-only queries to gather information
- Never executes write/DDL operations
- Produces a structured plan (saved as markdown)
- User can review, edit, then execute the plan
- Plan can be saved and shared (ticket, PR, Slack)
- Switch to plan mode: `\plan` or `Shift-Tab` (Claude Code style)

#### YOLO Mode

The AI executes within its autonomy level without asking. For power users who trust the agent.

```
alpha yolo> fix the bloat on the orders table
-- Running: ANALYZE orders; ✓
-- Running: REINDEX CONCURRENTLY orders_pkey; ✓
-- Running: REINDEX CONCURRENTLY idx_orders_created_at; ✓
-- Running: VACUUM orders; ✓
-- Done. Bloat reduced from 34% to 2%.
```

- Auto-executes anything within the configured autonomy level
- Still respects L1-L5 boundaries (YOLO + L3 = auto-runs safe ops, still asks for DROP)
- Shows what it's doing in real-time
- Ctrl-C aborts the current action
- `\yolo` to enter, `\interactive` to exit
- **Cannot be combined with L5** without explicit `--i-know-what-im-doing` flag

#### Observe Mode

Read-only. The AI watches and reports but never executes anything. For learning and auditing.

```
alpha observe> watch the database for 5 minutes
-- Observing...
-- 13:04:12 | 247 active connections (pool: 85% utilized)
-- 13:04:12 | Top wait event: LWLock:BufferContent (23% of samples)
-- 13:04:45 | ⚠ Long query detected (45s): SELECT * FROM audit_log WHERE...
-- 13:05:01 | Autovacuum running on: orders, shipments
-- 13:06:30 | ⚠ Replication lag increased: 12MB → 45MB
-- 13:08:55 | Replication lag recovered: 45MB → 3MB
-- 13:09:12 | Session complete.

Summary:
- Connection pressure is high (consider increasing pool_size)
- BufferContent lock contention suggests shared_buffers may be undersized
- 1 long query may need optimization (audit_log sequential scan)
-- Save this observation? [Y/n]
```

- Pure read-only (not even ANALYZE)
- Great for learning a new database
- Continuous or time-boxed observation
- Produces summary with recommendations
- `\observe` to enter

### 8.3 Mode Matrix

Modes are orthogonal — any input mode works with any execution mode:

| | **Interactive** | **Plan** | **YOLO** | **Observe** |
|---|---|---|---|---|
| **SQL mode** | Classic psql + AI suggestions | N/A (SQL is explicit) | N/A (SQL is explicit) | Read-only psql |
| **AI mode** | AI generates, you approve | AI plans, you review | AI does everything | AI watches, you learn |

### 8.4 Prompt Indicators

The prompt tells you exactly what mode you're in:

```
mydb=>                   -- SQL + Interactive (default)
mydb ai>                 -- AI + Interactive
mydb plan>               -- AI + Plan
mydb yolo>               -- AI + YOLO
mydb observe>            -- Observe
mydb [L3]=>              -- SQL + Interactive, autonomy L3 shown
mydb [L3] ai>            -- AI + Interactive, autonomy L3
mydb [L3] yolo>          -- AI + YOLO, autonomy L3
```

### 8.5 Slash Commands for Mode Control

```
\ai                      -- switch to AI input mode
\sql                     -- switch to SQL input mode (default)
\plan                    -- enter plan execution mode
\yolo                    -- enter YOLO execution mode
\interactive             -- return to interactive execution mode (default)
\observe [duration]      -- enter observe mode (optional time limit)
\level L1|L2|L3|L4|L5   -- set autonomy level
\mode                    -- show current mode summary
```

### 8.6 CLI Flags

```bash
alpha --ai               # start in AI mode
alpha --plan             # start in plan mode
alpha --yolo --level L3  # YOLO with L3 autonomy
alpha --observe 30m      # observe for 30 minutes, then exit
```

### 8.7 Context Awareness Across Modes

Regardless of mode, the AI maintains context:

- **Schema cache** — knows all tables, columns, indexes, constraints
- **Session history** — remembers recent queries and results in this session
- **pg_ash data** — if available, knows recent wait events and query performance
- **Plan history** — can reference previous plans ("execute step 3 from the last plan")
- **Error context** — remembers recent errors for follow-up questions

When switching modes, context carries over. A plan generated in plan mode can be executed in YOLO mode. An observation from observe mode can be investigated in AI mode.

---

## 9. Open Questions

1. **Name:** "Project Alpha" is the codename. Final shipping name TBD.
2. **License:** Source-available? Dual license (AGPL + commercial)? Apache 2.0? Decision impacts adoption and business model.
3. **Wire protocol:** Fork `tokio-postgres` or build from scratch? Start with tokio-postgres, evaluate after Phase 0.
4. **pgBouncer transaction mode:** How to handle features that require session-level state (prepared statements, temp tables) through poolers?
5. **Offline AI:** Should we bundle a small local model (e.g., quantized Phi-3) for environments without internet? Or is Ollama sufficient?
6. **Multi-database:** Should daemon mode monitor multiple databases from one process, or one process per database?
7. **Plugin API stability:** When do we commit to a stable plugin interface for custom connectors?

---

*This is a living document. Update as decisions are made and requirements evolve.*
