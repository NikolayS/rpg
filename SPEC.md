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

#### FR-1: Postgres Wire Protocol and Connection

**Wire protocol:**
- Wire protocol v3 (simple query, extended query protocol)
- Connect via TCP and Unix domain sockets
- Authentication: password, md5, SCRAM-SHA-256
- SSL/TLS via rustls (with native-tls fallback option)
- GSS encryption support (GSSAPI/Kerberos environments)
- Connection parameter negotiation
- CancelRequest for query cancellation
- COPY sub-protocol (both directions)
- LISTEN/NOTIFY async notification handling
- Large object streaming protocol

**Connection parameters (all libpq-compatible):**
- host, hostaddr, port, dbname, user, password
- sslmode (disable, allow, prefer, require, verify-ca, verify-full)
- sslcert, sslkey, sslrootcert, sslcrl, sslcrldir
- sslnegotiation, sslcompression, sslcertmode, sslsni
- ssl_min_protocol_version, ssl_max_protocol_version
- application_name, options (runtime parameters)
- connect_timeout, client_encoding
- target_session_attrs (any, read-write, read-only, primary, standby, prefer-standby)
- load_balance_hosts
- channel_binding, require_auth
- gssencmode, krbsrvname, gsslib, gssdelegation
- passfile, service (pg_service.conf)
- requirepeer (Unix socket peer auth)

**Environment variables (full libpq set):**
- PGHOST, PGHOSTADDR, PGPORT, PGDATABASE, PGUSER
- PGPASSWORD, PGPASSFILE
- PGOPTIONS — runtime options passed to server (e.g., `-c search_path=myschema`)
- PGAPPNAME — application_name
- PGSSLMODE, PGREQUIRESSL (deprecated), PGSSLCOMPRESSION
- PGSSLCERT, PGSSLKEY, PGSSLCERTMODE, PGSSLROOTCERT
- PGSSLCRL, PGSSLCRLDIR, PGSSLSNI
- PGSSLNEGOTIATION
- PGSSLMINPROTOCOLVERSION, PGSSLMAXPROTOCOLVERSION
- PGSERVICE, PGSERVICEFILE
- PGREQUIREAUTH, PGCHANNELBINDING
- PGGSSENCMODE, PGKRBSRVNAME, PGGSSLIB, PGGSSDELEGATION
- PGCONNECT_TIMEOUT
- PGCLIENTENCODING
- PGTARGETSESSIONATTRS, PGLOADBALANCEHOSTS
- PSQLRC, PSQL_HISTORY — psql-specific (we respect these for compatibility)
- PAGER, PSQL_PAGER — pager program selection
- PGTZ — default timezone
- PGDATESTYLE — default date style

**Connection string formats:**
- URI: `postgresql://user:pass@host:port/db?sslmode=require&options=-csearch_path%3Dmyschema`
- Key-value: `host=localhost port=5432 dbname=mydb sslmode=require options='-c search_path=myschema'`
- Positional: `alpha dbname user host port`

**Service file support:**
- `~/.pg_service.conf` and `PGSERVICEFILE`
- `pg_service.conf` in sysconfdir
- `\c service=myservice`

**`.pgpass` file support:**
- Standard location: `~/.pgpass` (Linux/macOS), `%APPDATA%\postgresql\pgpass.conf` (Windows)
- `PGPASSFILE` override
- Format: `hostname:port:database:username:password`
- Wildcard (`*`) support
- Permission check (600 on Unix)

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

**Tier 3 — Complete compatibility (Phase 2+):**
| Command | Description |
|---------|-------------|
| `\lo_import`, `\lo_export`, `\lo_list`, `\lo_unlink` | Large object commands |
| `\crosstabview [colV [colH [colD [sortcolH]]]]` | Pivot query results into crosstab grid |
| `\gdesc` | Describe result columns without executing |
| `\bind [params...]` | Bind parameters for next query (extended query protocol) |
| `\bind_named stmt [params...]` | Bind to named prepared statement |
| `\parse stmt` | Parse and save a prepared statement |
| `\close_prepared stmt` | Close a prepared statement |
| `\C [title]` | Set table title/caption |
| `\copyright` | Show PostgreSQL copyright |
| `\errverbose` | Show most recent error in verbose form |
| `\gx [file]` | Execute query with expanded output |

**Variable interpolation (full psql compatibility):**
- `:variable` — substitute variable value in SQL and meta-command arguments
- `:'variable'` — substitute as quoted literal
- `:"variable"` — substitute as quoted identifier
- `:{?variable}` — test if variable is defined (TRUE/FALSE)
- Backquote expansion: `` `command` `` — substitute shell command output
- Colon escaping: `\:` to prevent substitution

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

#### FR-11a: Permission Model (Database-Level Enforcement)

Autonomy levels (FR-11) define what the **application** considers allowed. But the real enforcement happens at the **Postgres level** through the privilege system. The tool connects as a specific database user, and that user's GRANTs define the hard boundary of what's possible — regardless of what the application-level autonomy config says.

**Principle:** Two-layer security. The application layer (L1-L5) is a policy filter. The database layer (GRANT/REVOKE + wrapper functions) is the enforcement mechanism. Even if the app layer has a bug, the database won't let the tool do anything its user can't do.

**How it works:**

1. **Dedicated database role** — the tool connects as a purpose-built role (e.g., `alpha_agent`), not as a superuser, not as the application owner.

2. **Fine-grained GRANTs** — the DBA grants exactly what the tool is allowed to do:
   ```sql
   -- L3: can ANALYZE, VACUUM, REINDEX CONCURRENTLY, cancel queries
   GRANT pg_stat_scan_tables TO alpha_agent;
   GRANT USAGE ON SCHEMA public TO alpha_agent;
   GRANT SELECT ON ALL TABLES IN SCHEMA public TO alpha_agent;
   -- But NOT: CREATE, DROP, ALTER, TRUNCATE, DELETE, INSERT, UPDATE
   ```

3. **PL/pgSQL wrapper functions with SECURITY DEFINER** — for operations that need elevated privileges but should be constrained:
   ```sql
   -- Wrapper: allows REINDEX CONCURRENTLY on any index, but nothing else
   CREATE OR REPLACE FUNCTION alpha_ops.reindex_concurrently(p_index regclass)
   RETURNS void
   LANGUAGE plpgsql
   SECURITY DEFINER  -- runs as the function owner (superuser/owner)
   SET search_path = pg_catalog
   AS $$
   BEGIN
     -- Validate: only indexes, not tables
     IF NOT EXISTS (SELECT 1 FROM pg_class WHERE oid = p_index AND relkind = 'i') THEN
       RAISE EXCEPTION 'Not an index: %', p_index;
     END IF;
     EXECUTE format('REINDEX INDEX CONCURRENTLY %I.%I',
       (SELECT nspname FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace WHERE c.oid = p_index),
       (SELECT relname FROM pg_class WHERE oid = p_index));
   END;
   $$;
   GRANT EXECUTE ON FUNCTION alpha_ops.reindex_concurrently(regclass) TO alpha_agent;
   ```

4. **dblink / postgres_fdw for non-transactional operations** — some operations (like `REINDEX CONCURRENTLY`, `CREATE INDEX CONCURRENTLY`, `VACUUM`) cannot run inside a transaction block. Wrapper functions use `dblink` or `postgres_fdw` to execute these in a separate connection:
   ```sql
   -- Wrapper using dblink for operations that can't run in a transaction
   CREATE OR REPLACE FUNCTION alpha_ops.vacuum_table(p_table regclass)
   RETURNS void
   LANGUAGE plpgsql
   SECURITY DEFINER
   AS $$
   DECLARE
     v_schema text;
     v_table text;
   BEGIN
     SELECT nspname, relname INTO v_schema, v_table
     FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
     WHERE c.oid = p_table AND c.relkind IN ('r', 'm');
     IF NOT FOUND THEN
       RAISE EXCEPTION 'Not a table: %', p_table;
     END IF;
     PERFORM dblink('dbname=' || current_database(),
       format('VACUUM %I.%I', v_schema, v_table));
   END;
   $$;
   ```

5. **Dynamic wrapper generation** — during the setup/permission review phase, the tool can generate the appropriate wrapper functions based on the desired autonomy level:
   ```
   alpha setup --level L3 --generate-wrappers
   -- Outputs SQL to create the alpha_ops schema, role, wrapper functions, and GRANTs
   -- DBA reviews and applies
   
   alpha setup --level L4 --generate-wrappers
   -- Generates additional wrappers for CREATE/DROP INDEX CONCURRENTLY, VACUUM FULL, etc.
   ```

6. **Permission introspection** — the tool checks what it can actually do on connect:
   ```
   alpha=> \permissions
   Role: alpha_agent
   Database: production
   
   Effective permissions:
     ✓ SELECT on all tables (public, analytics)
     ✓ ANALYZE (via alpha_ops.analyze_table)
     ✓ VACUUM (via alpha_ops.vacuum_table)
     ✓ REINDEX CONCURRENTLY (via alpha_ops.reindex_concurrently)
     ✓ pg_cancel_backend (via alpha_ops.cancel_query)
     ✗ CREATE INDEX — not granted (would need L4 wrappers)
     ✗ DROP — not granted
     ✗ ALTER SYSTEM — not granted
     ✗ pg_terminate_backend — not granted
   
   Autonomy config: L3
   Effective autonomy: L3 (all L3 operations available)
   ```

7. **Autonomy level clamping** — if the app config says L4 but the database role only has L3 permissions, the tool operates at L3 and warns:
   ```
   WARNING: Autonomy level L4 requested but database role 'alpha_agent' 
   lacks permissions for L4 operations (CREATE INDEX, DROP INDEX, VACUUM FULL).
   Effective autonomy: L3
   Run 'alpha setup --level L4 --generate-wrappers' to generate the required SQL.
   ```

**Schema layout:**
```
alpha_ops                    -- dedicated schema for wrapper functions
├── analyze_table(regclass)  -- SECURITY DEFINER, L3
├── vacuum_table(regclass)   -- SECURITY DEFINER + dblink, L3
├── reindex_concurrently(regclass)  -- SECURITY DEFINER + dblink, L3
├── cancel_query(int)        -- SECURITY DEFINER, L3
├── create_index_concurrently(text)  -- SECURITY DEFINER + dblink, L4
├── drop_index_concurrently(regclass)  -- SECURITY DEFINER + dblink, L4
├── vacuum_full_table(regclass)  -- SECURITY DEFINER + dblink, L4
├── terminate_backend(int)   -- SECURITY DEFINER, L4
├── alter_system_set(text, text)  -- SECURITY DEFINER, L4 (with parameter allowlist)
├── reload_conf()            -- SECURITY DEFINER, L3
└── ...                      -- dynamically generated based on level
```

**Why this matters:**
- **Cloud environments** (RDS, Cloud SQL, Supabase) don't give superuser access. Wrapper functions work within managed Postgres constraints.
- **SOC2/compliance** — the audit trail is in Postgres's own `pg_audit` log, not just the app log. The database itself enforces what the tool can do.
- **Defense in depth** — a prompt injection that tricks the AI layer into generating a `DROP TABLE` will fail at the database level because the role can't execute it.
- **Gradual trust** — start with L1 (read-only role, no wrappers), add wrappers as trust builds, same as the autonomy philosophy.

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
- Human at the terminal, full REPL experience
- Readline, autocomplete, syntax highlighting, TUI pager
- AI assists in real-time (when configured)
- Agent suggestions appear inline
- Detects TTY automatically — if stdin is a terminal, interactive mode
- `--interactive` / `-i` flag to force interactive even when piping

**Non-interactive mode:**
- Activated automatically when stdin is not a TTY (piped input, `-c`, `-f`)
- No readline, no autocomplete, no highlighting, no pager
- Output is raw, machine-parseable (respects format flags: `-A`, `-t`, `-P format=csv`, etc.)
- Errors go to stderr
- Exit codes reflect success/failure (match psql: 0/1/2/3)
- Suitable for cron jobs, scripts, CI/CD pipelines
- `\echo`, `\qecho` still work for scripted output

**Daemon mode:**
- `alpha daemon --config config.toml`
- Runs headless, no REPL, no stdin
- Continuous monitoring loop
- Reports via configured channels (Slack webhook, email, GitHub issues)
- PID file, systemd unit support
- Health check endpoint (HTTP)
- Graceful shutdown on SIGTERM

**Single-shot mode:**
- `-c "SQL"` — execute single command and exit
- `-f file` — execute file and exit
- `--check` — run health check, exit code = severity (0=healthy, 1=warning, 2=critical)
- `--report [format]` — full diagnostic report to stdout/file (text, json, markdown)

#### FR-14: Debug Mode and Logging

**Debug flag:**
- `--debug` / `-D` — enable debug mode
- `\set DEBUG on|off` — toggle at runtime in interactive mode
- `ALPHA_DEBUG=1` environment variable
- Default: off

**What debug mode does:**
- Logs all wire protocol messages (sent and received) with timestamps
- Logs all SQL queries as sent to the server (including `\d`-generated queries)
- Logs connection negotiation details (auth method, SSL handshake, parameters)
- Logs backslash command parsing and dispatch
- Logs autocomplete cache refreshes and schema introspection queries
- Logs AI requests and responses (with token counts)
- Logs agent actions and decisions (with justifications)
- Logs connector API calls (URLs, status codes, latency — never credentials)

**Log destinations:**
- **stderr** — when `--debug` is used in interactive mode, debug output goes to stderr (doesn't pollute query results on stdout)
- **Log file** — `--log-file path` or config `logging.file` — always append, never truncate
- **Default log location:** `~/.local/share/alpha/debug.log` (when log file enabled)
- **Structured format:** `[timestamp] [level] [component] message`

**Log levels:**
- `error` — connection failures, unrecoverable errors
- `warn` — degraded behavior, fallback paths, deprecated usage
- `info` — connection events, mode changes, significant actions (default for log file)
- `debug` — everything above plus protocol messages, query details, AI interactions
- `trace` — everything above plus raw byte-level wire protocol dumps

**Configuration:**
```toml
[logging]
level = "info"                              # stderr threshold (interactive)
file = "~/.local/share/alpha/debug.log"     # log file path (empty = disabled)
file_level = "debug"                        # log file threshold
action_log = "~/.local/share/alpha/actions.log"  # agent action audit log (separate)
max_file_size_mb = 100                      # rotate at this size
max_files = 5                               # keep N rotated files
```

**CLI flags:**
```bash
alpha --debug                    # debug to stderr
alpha --debug --log-file out.log # debug to stderr + file
alpha --log-file out.log         # info to file, no debug on stderr
alpha --log-level trace          # maximum verbosity
alpha -D -E                      # debug mode + echo hidden queries (psql -E compat)
```

**Interaction with psql flags:**
- `-E` / `--echo-hidden` — show queries generated by `\d` commands (psql compat, works without debug mode)
- `-e` / `--echo-queries` — echo all queries sent to server (psql compat)
- `-b` / `--echo-errors` — echo failed commands (psql compat)
- `--debug` is a superset: enables all echo flags plus protocol/internal logging

**Security:**
- Debug logs never contain passwords, API keys, or auth tokens
- Credentials are masked in connection string logs: `postgresql://user:****@host:5432/db`
- AI API keys are never logged; only provider name and model are recorded
- Log files are created with 600 permissions

#### FR-15: Session Management

Borrowed from Claude Code and OpenClaw. Long-running database work needs session continuity.

**Sessions:**
- Each interactive session gets a unique ID and is persisted
- Session includes: connection parameters, query history, AI conversation, variables, mode state
- `\session list` — list recent sessions with timestamps, database, duration
- `\session resume [id]` — resume a previous session (reconnects, restores variables and history)
- `\session save [name]` — save current session with a name
- `\session delete [id]` — delete a session
- Storage: SQLite database at `~/.local/share/alpha/sessions.db`

**Context compaction (from Claude Code / OpenClaw):**
- AI conversation context grows over a session — queries, results, explanations accumulate
- When context approaches model's token limit, auto-compact: summarize older conversation, keep recent
- `/compact` — manually trigger compaction with optional focus ("compact, keep focus on performance tuning")
- `/clear` — clear AI conversation context entirely (keep connection, variables, history)
- Compaction summary is persisted in session for resume

**Undo:**
- `\undo` — undo the last AI-executed action
- Only works for DDL/DML that the AI executed (not manual SQL)
- Generates and runs the reverse operation where possible:
  - `CREATE INDEX` → `DROP INDEX`
  - `ALTER TABLE ADD COLUMN` → `ALTER TABLE DROP COLUMN`
  - `INSERT` → `DELETE` (if PK available)
  - For non-reversible operations (DROP, TRUNCATE): warns that undo is not possible
- Maintains an undo stack per session (configurable depth, default 20)
- `\undo list` — show undo stack
- `\undo all` — undo all AI-executed actions in reverse order

#### FR-16: Named Queries (Favorites)

Borrowed from pgcli. Save frequently used queries with short names.

```
-- Save a query
\ns active_locks SELECT pid, relation::regclass, mode, granted FROM pg_locks WHERE NOT granted;

-- List all saved queries
\n+

-- Execute a saved query
\n active_locks

-- Delete a saved query
\nd active_locks

-- Print a saved query without executing
\np active_locks

-- Save with parameters (positional)
\ns top_tables SELECT * FROM pg_stat_user_tables ORDER BY $1 DESC LIMIT $2;
\n top_tables seq_scan 10
```

- Stored in `~/.config/alpha/named_queries.toml` (portable, shareable)
- Support positional parameters (`$1`, `$2`, ...)
- Tab-completion for query names
- Can be shared across team via version-controlled config

#### FR-17: Destructive Statement Protection

Borrowed from pgcli. Warn before executing dangerous statements.

```
alpha=> DROP TABLE users;
WARNING: This is a destructive operation.
Are you sure you want to execute: DROP TABLE users? [y/N]
```

**Protected statements (configurable):**
- `DROP TABLE`, `DROP DATABASE`, `DROP SCHEMA`, `DROP INDEX` (without CONCURRENTLY)
- `TRUNCATE`
- `DELETE` without `WHERE`
- `UPDATE` without `WHERE`
- `ALTER TABLE ... DROP COLUMN`
- `ALTER SYSTEM RESET ALL`

**Configuration:**
```toml
[safety]
destructive_warning = true              # enable/disable
destructive_statements_require_transaction = false  # require explicit transaction for destructive ops
protected_patterns = [                  # custom patterns (regex)
  "DROP\\s+TABLE",
  "TRUNCATE",
  "DELETE\\s+FROM\\s+\\w+\\s*;",        # DELETE without WHERE
]
```

- In YOLO mode: warnings still fire for operations above the autonomy level
- In non-interactive mode: destructive statements abort with error unless `--force` flag

#### FR-18: Keybindings

Borrowed from pgcli. Configurable keybinding modes.

**Emacs mode (default):**
- Standard emacs keybindings: Ctrl-A (home), Ctrl-E (end), Ctrl-K (kill line), etc.

**Vi mode:**
- Modal editing: Esc for normal mode, i for insert mode
- `^` (beginning), `$` (end), `w` (word forward), `b` (word back), etc.

**Toggle:** F4 key or `\set VI on|off`

**Function keys (pgcli-inspired):**
| Key | Function |
|-----|----------|
| F2 | Toggle smart completion on/off |
| F3 | Toggle multi-line mode on/off |
| F4 | Toggle Vi/Emacs keybinding mode |
| F5 | Toggle auto-EXPLAIN on/off (pgcli-style: auto-prepend EXPLAIN to queries) |
| Ctrl-T | Toggle SQL/text2sql input mode |
| Ctrl-R | Reverse history search |
| Ctrl-Space | Force autocomplete |
| Tab | Autocomplete (on non-empty line) |
| Alt-Enter | Insert newline (multi-line mode, emacs) |

**Custom keybindings (config file):**
```toml
[keybindings]
mode = "emacs"    # emacs | vi
custom = [
  { key = "ctrl-p", action = "history_prev" },
  { key = "ctrl-n", action = "history_next" },
]
```

#### FR-19: Smart Autocomplete

Enhanced autocomplete beyond basic schema objects. Borrowed from pgcli with additions.

**Smart vs. basic completion:**
- **Smart (default):** Context-sensitive — only suggest relevant items based on SQL position
- **Basic:** Show all possible completions regardless of context
- Toggle with F2

**Features:**
- Fuzzy matching: typing `djmi` matches `django_migrations` (pgcli-style)
- Alias resolution: `SELECT u. FROM users u` → suggests columns of `users`
- Schema qualification: `public.` → only objects in `public` schema
- Keyword casing: auto-detect and match user's casing style (configurable: lower/upper/auto)
- Table alias generation: optionally auto-suggest aliases when completing table names (`users` → `users u`)
- Cross-schema search: when no schema prefix, search all schemas in `search_path`
- CTE/subquery awareness: autocomplete columns from CTEs and subqueries
- Function signature hints: show parameter types when completing function names
- Completion for GUC parameter names after `SET` / `ALTER SYSTEM SET`
- Completion for enum values in `WHERE col = '...'` context

#### FR-20: Project Configuration Files

Borrowed from Claude Code (CLAUDE.md/AGENTS.md) and OpenCode (/init).

**`.alpha.toml`** — project-level configuration, checked into git:
```toml
# .alpha.toml — project-level config (lives in repo root)
[connection]
default_database = "myapp_development"
default_host = "localhost"

[named_queries]
migrations = "SELECT * FROM schema_migrations ORDER BY version DESC LIMIT 20"
active = "SELECT * FROM pg_stat_activity WHERE state = 'active'"

[ai]
context_files = ["docs/schema.md", "docs/queries.md"]  # extra context for AI
system_prompt = "This is a Rails app. The schema uses UUID primary keys."

[safety]
protected_tables = ["users", "payments", "audit_log"]  # extra protection for these tables
```

**`POSTGRES.md`** — natural language project context (like AGENTS.md):
```markdown
# Database Context

This is a Rails 7 application using PostgreSQL 16.

## Schema conventions
- All tables use UUID primary keys
- Soft deletes via `deleted_at` column
- Audit trail in `audit_log` table — never DELETE from this table

## Known issues
- The `orders` table has significant bloat, VACUUM regularly
- Index `idx_orders_legacy` is unused, safe to drop
```

- AI reads these files on connect (if present in current directory or home)
- `/init` command: AI analyzes the connected database and generates `.alpha.toml` and `POSTGRES.md`

#### FR-21: Multi-line Mode

Borrowed from pgcli. Configurable behavior for Enter key.

**Modes:**
- **psql mode (default):** Enter executes if line ends with semicolon, otherwise continues
- **safe mode:** Enter always inserts newline; Esc+Enter or Alt+Enter executes
- **single-line mode:** Enter always executes (psql `-S` compat)

**Toggle:** F3 or `\set MULTILINE psql|safe|single`

**Configuration:**
```toml
[input]
multi_line = true                # enable multi-line (default)
multi_line_mode = "psql"         # psql | safe
```

#### FR-22: SSH Tunnel Support

Borrowed from pgcli. Built-in SSH tunnel for remote databases.

```bash
# Connect through SSH tunnel
alpha --ssh-tunnel user@bastion:22 -h db-host -p 5432 -d mydb

# Using config
alpha -h mydb@production   # resolves from named connections with tunnel config
```

**Configuration:**
```toml
[connections.production]
host = "10.0.1.5"
port = 5432
database = "myapp"
user = "app_user"
ssh_tunnel = { host = "bastion.example.com", port = 22, user = "deploy", key = "~/.ssh/id_ed25519" }
```

- Automatic local port allocation
- SSH agent forwarding support
- Key-based and password auth
- Keep-alive for long sessions

#### FR-23: Query Audit Log

Separate from debug logging. A user-friendly log of all queries executed, for compliance and review.

```
\log-file queries.log        # start logging all queries + results to file
\log-file                    # stop logging
```

**Audit log format:**
```
-- 2026-03-12 14:23:01 UTC | mydb | user=nik | duration=12ms
SELECT * FROM users WHERE id = 42;
-- (1 row)

-- 2026-03-12 14:23:15 UTC | mydb | user=nik | duration=340ms | source=text2sql
-- prompt: "show me users who signed up this week"
SELECT * FROM users WHERE created_at >= date_trunc('week', current_date);
-- (47 rows)

-- 2026-03-12 14:24:02 UTC | mydb | user=nik | duration=2100ms | source=agent:L3
-- action: REINDEX CONCURRENTLY idx_orders_created_at
-- justification: Index bloat at 34%, threshold 25%
SELECT alpha_ops.reindex_concurrently('idx_orders_created_at'::regclass);
-- OK
```

- Every query logged with: timestamp, database, user, duration, source (manual/text2sql/agent)
- Agent actions include justification
- Configurable: `logging.audit_file` in config
- Separate from debug log — audit is human-readable, debug is machine-verbose

#### FR-24: Notification and Alert Channels

For daemon mode and background monitoring. Borrowed from OpenClaw.

**Channels:**
- Slack webhook
- Email (SMTP)
- PagerDuty
- Generic webhook (POST JSON to URL)
- Telegram bot
- stdout/stderr (for container logging)

**Configuration:**
```toml
[alerts]
channels = ["slack", "email"]

[alerts.slack]
webhook_url_env = "SLACK_WEBHOOK_URL"
channel = "#db-alerts"
severity_threshold = "warning"   # only send warning+ severity

[alerts.email]
smtp_host = "smtp.example.com"
smtp_port = 587
from = "alpha@example.com"
to = ["dba@example.com"]
severity_threshold = "critical"  # only critical alerts via email

[alerts.pagerduty]
routing_key_env = "PD_ROUTING_KEY"
severity_threshold = "critical"
```

**Alert format:**
```json
{
  "severity": "warning",
  "database": "production",
  "host": "db-01.example.com",
  "check": "index_bloat",
  "message": "Index idx_orders_created_at bloat at 34% (threshold: 25%)",
  "recommendation": "REINDEX CONCURRENTLY idx_orders_created_at",
  "autonomy_action": "auto-reindex scheduled (L3)",
  "timestamp": "2026-03-12T14:30:00Z"
}
```

#### FR-25: Status Bar / Status Line

Borrowed from Claude Code. A persistent status line at the bottom of the terminal.

**Displays:**
- Connection: `db-host:5432/mydb` (green=connected, red=disconnected, yellow=reconnecting)
- Mode: `SQL` | `text2sql` | `plan` | `yolo` | `observe`
- Autonomy: `L1` | `L2` | `L3` | `L4` | `L5`
- Transaction state: idle | in-transaction | failed
- Query timing: last query duration
- AI: token usage / budget remaining (when AI is active)
- Latency: connection RTT
- Replication lag (if connected to replica and monitoring)

**Customizable:** config or `\set STATUSLINE` format string:
```toml
[display]
statusline = "{host}:{port}/{db} | {mode} | {autonomy} | {tx_state} | {last_duration}"
```

**Toggle:** `\set STATUSLINE on|off`

#### FR-26: Explain Mode (Auto-EXPLAIN)

Borrowed from pgcli's F5 feature. When enabled, automatically prepends EXPLAIN to every query.

```
alpha=> \set EXPLAIN on
-- Explain mode ON. All queries will show execution plan.

alpha=> SELECT * FROM users WHERE email = 'test@example.com';
                          QUERY PLAN
--------------------------------------------------------------
 Index Scan using idx_users_email on users  (cost=0.42..8.44 rows=1 width=128)
   Index Cond: (email = 'test@example.com'::text)
(2 rows)
```

**Variants:**
- `\set EXPLAIN on` — EXPLAIN only (no execution)
- `\set EXPLAIN analyze` — EXPLAIN ANALYZE (executes the query)
- `\set EXPLAIN verbose` — EXPLAIN (ANALYZE, VERBOSE, BUFFERS, TIMING)
- F5 to toggle through: off → explain → analyze → verbose → off

**AI integration:** When explain mode is on and AI is active, automatically feed the plan to the LLM for interpretation.

#### FR-27: Connection Profiles

Named connections with full configuration, including tunnels and autonomy settings.

```toml
[connections.local]
host = "localhost"
port = 5432
database = "myapp_dev"
user = "dev"

[connections.staging]
host = "staging-db.internal"
port = 5432
database = "myapp"
user = "readonly"
sslmode = "require"
autonomy = "L1"      # read-only on staging

[connections.production]
host = "10.0.1.5"
port = 5432
database = "myapp"
user = "alpha_agent"
sslmode = "verify-full"
sslrootcert = "~/.ssl/rds-ca.pem"
autonomy = "L3"
ssh_tunnel = { host = "bastion.prod.example.com", user = "deploy" }
```

**Usage:**
```
alpha @local          # connect using 'local' profile
alpha @production     # connect using 'production' profile (with SSH tunnel)
\c @staging           # switch to staging profile mid-session
```

- Tab-completion for profile names
- `\profiles` — list all configured profiles
- Autonomy level can be pinned per profile (production ≠ development)

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
- [ ] Connection: full libpq-compatible parameter parsing
  - [ ] All connection parameters (host, hostaddr, port, dbname, user, password, sslmode, sslcert, sslkey, sslrootcert, application_name, options, connect_timeout, client_encoding, target_session_attrs, etc.)
  - [ ] URI format: `postgresql://user:pass@host:port/db?sslmode=require&options=...`
  - [ ] Key-value format: `host=... port=... dbname=...`
  - [ ] Positional arguments: `alpha dbname user host port`
- [ ] All libpq environment variables:
  - [ ] PGHOST, PGHOSTADDR, PGPORT, PGDATABASE, PGUSER, PGPASSWORD, PGPASSFILE
  - [ ] PGOPTIONS, PGAPPNAME, PGSSLMODE, PGSSLCERT, PGSSLKEY, PGSSLROOTCERT
  - [ ] PGSSLCRL, PGSERVICE, PGSERVICEFILE, PGREQUIREAUTH, PGCHANNELBINDING
  - [ ] PGGSSENCMODE, PGKRBSRVNAME, PGCONNECT_TIMEOUT, PGCLIENTENCODING
  - [ ] PGTARGETSESSIONATTRS, PGLOADBALANCEHOSTS, PGTZ, PGDATESTYLE
  - [ ] PSQLRC, PSQL_HISTORY, PAGER, PSQL_PAGER
- [ ] `.pgpass` file support (standard paths, PGPASSFILE, wildcard, permission check)
- [ ] `pg_service.conf` support (PGSERVICE, PGSERVICEFILE, ~/.pg_service.conf)
- [ ] SSL/TLS (rustls + native-tls fallback)
- [ ] Auth: password, md5, SCRAM-SHA-256
- [ ] Unix domain sockets (Linux/macOS)
- [ ] Wire protocol v3: simple query, extended query protocol
- [ ] Basic REPL: rustyline loop, persistent history file, multi-line input (semicolons)
- [ ] Query execution with result display (aligned format with headers)
- [ ] Row count footer ("(N rows)")
- [ ] Query timing display
- [ ] Error display with SQLSTATE, detail, hint, context, position
- [ ] `\errverbose` — show most recent error in verbose form

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
  - [ ] `\copy ... FROM stdin` / `\copy ... TO stdout`
  - [ ] `\copy ... FROM program 'cmd'` / `\copy ... TO program 'cmd'`
  - [ ] pstdin/pstdout support
- [ ] `\watch [interval]` — re-execute query periodically
- [ ] `\g [file]` — execute query, optionally send to file
- [ ] `\g |command` — pipe query output to shell command
- [ ] `\gx [file]` — execute query with expanded output
- [ ] `\gset [prefix]` — execute query, store results as variables
- [ ] `\gexec` — execute each result value as a SQL statement
- [ ] `\gdesc` — describe result columns without executing
- [ ] `\sf[+] function` — show function source
- [ ] `\sv[+] view` — show view definition
- [ ] `\bind [params...]` — set bind parameters for next query (extended query protocol)
- [ ] `\bind_named stmt [params...]` — bind to named prepared statement
- [ ] `\parse stmt` — parse and save a prepared statement
- [ ] `\close_prepared stmt` — close a prepared statement
- [ ] `\crosstabview [colV [colH [colD [sortcolH]]]]` — pivot results
- [ ] `\copyright` — show PostgreSQL copyright
- [ ] `\errverbose` — show most recent error in verbose form
- [ ] `\C [title]` — set table title
- [ ] Variable interpolation: `:var`, `:'var'`, `:"var"`, `:{?var}`, backquote expansion
- [ ] Output formats: aligned (default), unaligned, wrapped, CSV, HTML, LaTeX, LaTeX-longtable, JSON, asciidoc, troff-ms
- [ ] Customizable null display, border, line style (ascii/old-ascii/unicode), unicode_border_linestyle, unicode_column_linestyle, unicode_header_linestyle
- [ ] `\pset` options: format, border, expanded, fieldsep, fieldsep_zero, footer, null, numericlocale, recordsep, recordsep_zero, title, tuples_only, pager, pager_min_lines, unicode_*
- [ ] `-c "SQL"` — execute single command and exit
- [ ] `-f file` — execute file and exit
- [ ] `-v name=value` — set variable from command line
- [ ] `-X` — skip .psqlrc
- [ ] `-A` — unaligned output
- [ ] `-t` — tuples only
- [ ] `-F sep` — field separator for unaligned output
- [ ] `-R sep` — record separator for unaligned output
- [ ] `-P option=value` — set pset from command line
- [ ] `-o file` — output to file
- [ ] `-L file` — send log of all query output to file (in addition to normal output)
- [ ] `-1` / `--single-transaction` — wrap `-f` in BEGIN/COMMIT
- [ ] `-b` / `--echo-errors` — echo failed commands
- [ ] `-e` / `--echo-queries` — echo all queries sent
- [ ] `-E` / `--echo-hidden` — show queries generated by `\d` commands
- [ ] `-n` / `--no-readline` — disable readline
- [ ] `-q` / `--quiet` — suppress informational messages
- [ ] `-s` / `--single-step` — single step mode (confirm each command)
- [ ] `-S` / `--single-line` — single-line mode (newline = semicolon)
- [ ] `-w` / `--no-password` — never prompt for password
- [ ] `-W` / `--password` — force password prompt
- [ ] `-z` / `--field-separator-zero` — zero byte field separator for unaligned
- [ ] `-0` / `--record-separator-zero` — zero byte record separator for unaligned
- [ ] `--csv` — CSV output mode
- [ ] `--json` — JSON output mode
- [ ] Stdin/stdout piping: `echo "SELECT 1" | alpha`
- [ ] Exit codes: 0 success, 1 error, 2 connection failure, 3 script error (match psql)
- [ ] `.psqlrc` execution on startup (skip with `-X`)
- [ ] PSQLRC environment variable
- [ ] `~/.psql_history` / PSQL_HISTORY for history file location
- [ ] Tab completion: SQL keywords, schema objects, file paths (for `\i`, `\copy`)
- [ ] Query cancellation: Ctrl-C sends CancelRequest to server
- [ ] Ctrl-D on empty line exits
- [ ] Customizable prompts (PROMPT1, PROMPT2, PROMPT3) with all format codes:
  - [ ] `%M` — full hostname, `%m` — short hostname, `%>` — port
  - [ ] `%n` — username, `%/` — database, `%~` — like %/ but ~ for default
  - [ ] `%#` — `#` if superuser else `>`, `%p` — PID of backend
  - [ ] `%R` — `=` for ready, `^` for single-line, `!` for disconnected
  - [ ] `%l` — line number, `%w` — whitespace of same width as prompt
  - [ ] `%x` — transaction status (`*` active, `!` failed, `?` unknown)
  - [ ] `%[` / `%]` — terminal control character brackets
- [ ] Notification (LISTEN/NOTIFY) display
- [ ] Transaction status in prompt
- [ ] Conditional commands: `\if`, `\elif`, `\else`, `\endif` — scripting conditionals

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
- No AI dependency — works fully offline, no API keys needed

#### text2sql Mode

Input is treated as natural language. The AI translates intent into SQL, shows it, and optionally executes.

```
alpha text2sql> show me the 10 biggest tables
-- Generating SQL...
SELECT schemaname, tablename, 
       pg_total_relation_size(schemaname || '.' || tablename) AS total_size
FROM pg_tables 
ORDER BY pg_total_relation_size(schemaname || '.' || tablename) DESC 
LIMIT 10;
-- Run this query? [Y/n/edit]

alpha text2sql> why is this query slow: SELECT * FROM orders WHERE created_at > now() - interval '1 day'
-- Analyzing...
-- The orders table has 12M rows but no index on created_at.
-- Currently doing a sequential scan (cost: 847291).
-- Recommendation: CREATE INDEX CONCURRENTLY idx_orders_created_at ON orders(created_at);
-- Create this index? [Y/n] (requires L3+ permissions)

alpha text2sql> fix index bloat on the orders table
-- Checking orders table indexes...
-- idx_orders_created_at: 34% bloat (450MB → should be ~300MB)
-- idx_orders_customer_id: 12% bloat (OK)
-- Plan:
--   1. SELECT alpha_ops.reindex_concurrently('idx_orders_created_at'::regclass);
-- Execute? [Y/n/edit]
```

- Prompt changes: `dbname text2sql>`
- Everything typed is interpreted as natural language
- AI generates SQL, **always shows it before executing** (unless in YOLO execution mode)
- Generated SQL respects the permission model — uses wrapper functions when direct access isn't available
- Can still run raw SQL by prefixing with `;` or `\sql`
- Tab completes common intents: "show me...", "why is...", "fix...", "optimize...", "compare..."
- Requires AI backend configured (errors clearly if not)

#### Switching Modes

```
-- From SQL mode:
\text2sql               -- switch to text2sql mode
\t2s                    -- short alias
-- or just prefix a single query:
/ask show me table sizes -- one-shot text2sql, stays in SQL mode

-- From text2sql mode:
\sql                    -- switch back to SQL mode
-- or prefix raw SQL:
;SELECT 1               -- one-shot SQL, stays in text2sql mode

-- Toggle:
Ctrl-T                  -- toggle between SQL and text2sql mode
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
| **SQL mode** | Classic psql (default) | N/A (SQL is explicit) | N/A (SQL is explicit) | Read-only psql |
| **text2sql mode** | AI generates, you approve | AI investigates, produces plan | AI does everything within permissions | AI watches, you learn |

### 8.4 Prompt Indicators

The prompt tells you exactly what mode you're in:

```
mydb=>                   -- SQL + Interactive (default)
mydb text2sql>           -- text2sql + Interactive
mydb plan>               -- text2sql + Plan
mydb yolo>               -- text2sql + YOLO
mydb observe>            -- Observe
mydb [L3]=>              -- SQL + Interactive, autonomy L3 shown
mydb [L3] text2sql>      -- text2sql + Interactive, autonomy L3
mydb [L3] yolo>          -- text2sql + YOLO, autonomy L3
```

### 8.5 Slash Commands for Mode Control

```
\text2sql / \t2s         -- switch to text2sql input mode
\sql                     -- switch to SQL input mode (default)
\plan                    -- enter plan execution mode
\yolo                    -- enter YOLO execution mode
\interactive             -- return to interactive execution mode (default)
\observe [duration]      -- enter observe mode (optional time limit)
\level L1|L2|L3|L4|L5   -- set autonomy level
\permissions             -- show effective permissions (role GRANTs + wrapper functions)
\mode                    -- show current mode summary (input mode + execution mode + autonomy + permissions)
```

### 8.6 CLI Flags

```bash
alpha --text2sql         # start in text2sql mode
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
