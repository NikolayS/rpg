# rpg

> The best terminal for diagnosing and fixing Postgres production issues.

**Status:** Active development (v0.3.0)
**Language:** Rust
**Org:** [DBLab Inc.](https://dblab.io)

---

## What is this?

A modern Postgres terminal that combines four things that have never existed in one tool:

1. **AI-powered terminal** — LLM inside, understands your schema, explains errors, writes and optimizes SQL, performs root cause analysis
2. **Batteries included** — pgcli-style autocomplete, built-in pager, postgres_dba diagnostics
3. **psql-compatible** — common psql workflows work out of the box, respecting 30 years of muscle memory
4. **DBA toolkit** — health checks, index analysis, bloat detection, vacuum monitoring, replication status

Think: `pgcli` for UX, `warp` for AI, `postgres_dba` built in.

---

## Why?

Every production Postgres incident follows the same painful loop: notice something's wrong, SSH in, run ad-hoc queries against pg_stat_activity, reconstruct what happened, figure out a fix, apply it, hope it works. This takes 30-60 minutes even for experienced DBAs.

AI coding tools (Cursor, Warp, Claude Code) proved that putting an LLM *inside* the tool you already use is transformative. Nobody's done this for Postgres.

**The opportunity:** Build the terminal where Postgres expertise meets AI assistance. The psql compatibility gets you in the door; the diagnostic and AI capabilities keep you there.

---

## Architecture

```
┌─────────────────────────────────────────────────────┐
│                       rpg                           │
│                                                     │
│  ┌─────────────┐  ┌──────────────┐  ┌───────────┐  │
│  │  AI Engine  │  │  UX Layer    │  │  Core     │  │
│  │             │  │              │  │           │  │
│  │ LLM engine  │  │ Autocomplete │  │ Wire proto│  │
│  │ NL → SQL    │  │ Highlighting │  │ \commands │  │
│  │ EXPLAIN     │  │ TUI pager    │  │ COPY/LOB  │  │
│  │ RCA chain   │  │ \dba diags   │  │ .psqlrc   │  │
│  │ /ask        │  │ Status bar   │  │ Formatting│  │
│  └─────────────┘  └──────────────┘  └───────────┘  │
└─────────────────────────────────────────────────────┘
```

---

## Core: psql-Compatible Terminal

A Postgres terminal compatible with common psql workflows, reimplemented in Rust.

### Goals
- **Postgres wire protocol v3** — connect, query, extended query protocol, SSL, SCRAM auth
- **Backslash meta-commands** — the top 50 commands that cover daily use: `\d`, `\l`, `\dt`, `\di`, `\c`, `\x`, `\timing`, `\copy`, `\e`, `\i`, `\set`, `\watch`, and more
- **Output formats** — aligned, unaligned, wrapped, CSV, HTML, LaTeX, JSON
- **Session state** — variables (`\set`/`\unset`), prompts, ON_ERROR behavior
- **COPY streaming** — `\copy` to/from with all format options
- **Tab completion** — SQL keywords, schema objects, file paths
- **Piping & scripting** — `-c`, `-f`, stdin/stdout, `\g`, `\gset`, `\gexec`

### Compatibility Policy
- **Interactive daily use:** target parity with the top 50 psql commands
- **Scripted automation:** only documented-compatible flags guaranteed
- **Unsupported behavior:** fails loudly, never silently
- 100% psql compatibility is a multi-year rabbit hole. Common workflows first, long tail later.

---

## DBA Diagnostics

Everything `postgres_dba` does, built in as first-class commands.

```
\dba activity     — current activity (pg_stat_activity)
\dba bloat        — table and index bloat
\dba locks        — lock trees and conflicts
\dba unused-idx   — unused indexes
\dba seq-scans    — tables with excessive seq scans
\dba cache-hit    — buffer cache hit ratio
\dba vacuum       — vacuum/autovacuum status
\dba replication  — replication lag and status
\dba indexes      — index health (unused, redundant, invalid, bloated)
\dba config       — non-default configuration parameters
\dba connections  — connection counts by state
\dba io           — I/O statistics by backend type (PG 16+)
```

---

## AI Terminal

An LLM lives inside the terminal.

### Capabilities
- **Natural language → SQL** — "show me the 10 biggest tables" → generates and optionally runs the query
- **Error explanation** — failed query? Get a human-readable diagnosis with fix suggestions
- **EXPLAIN analysis** — paste or run `EXPLAIN ANALYZE`, get plain-English breakdown with optimization suggestions
- **Schema-aware context** — the LLM knows your tables, columns, indexes, constraints
- **Query optimization** — suggest rewrites, missing indexes, better join strategies

### Interface
```
-- Natural language mode
rpg=> /ask show me the top 10 queries by total time

-- Inline explanation
rpg=> SELECT * FROM orders WHERE status = 'pending';
ERROR: column "status" does not exist
-- 💡 Did you mean "order_status"? (orders.order_status text NOT NULL)

-- EXPLAIN analysis
rpg=> /explain SELECT * FROM orders JOIN customers ON ...
-- Returns annotated plan with bottleneck identification
```

### LLM Backend
- Pluggable: OpenAI, Anthropic, local models (ollama)
- Context window management — schema + recent queries as context
- Streaming responses in terminal

---

## UX

### Schema-Aware Autocomplete
- pgcli-style dropdown with arrow key navigation
- Table/column/function/type names contextual to the query being written
- Keyword completion with Postgres version awareness

### Syntax Highlighting
- Real-time SQL highlighting in the input line
- Keywords, strings, numbers, operators, schema objects — each with distinct colors

### Integrated TUI Pager
- Replaces the need for an external pager (less/pspg)
- Column freezing, horizontal scroll, search
- Built with `ratatui`

### Status Bar
- Persistent bottom bar showing connection info, transaction state, query timing, AI token usage

---

## Daemon Mode

rpg can run headless as a background monitor:

```bash
rpg daemon --config /etc/rpg/config.toml
```

- Continuous health monitoring with anomaly detection
- Notification channels: Slack, Telegram, PagerDuty, webhooks
- Alert deduplication and severity-based routing
- Deployable as a sidecar container

---

## Roadmap

### Phase 0: Foundation ✅
- [x] Wire protocol client with auth (SCRAM, SSL, password)
- [x] Basic REPL with rustyline
- [x] Core backslash commands
- [x] Output formatting (aligned, \x expanded)
- [x] Basic autocomplete (keywords + schema objects)

### Phase 1: Daily Driver ✅
- [x] Remaining common backslash commands (\copy, \e, \i, \set, \watch)
- [x] Syntax highlighting
- [x] TUI pager (ratatui)
- [x] postgres_dba diagnostics as \dba commands
- [x] Single binary distribution

### Phase 2: AI Brain ✅
- [x] LLM integration framework (pluggable providers)
- [x] /ask command — NL → SQL
- [x] Error explanation with schema context
- [x] EXPLAIN ANALYZE interpreter
- [x] Context management (schema + session + history)

### Phase 3: Monitoring (current)
- [x] Daemon mode with anomaly detection
- [x] Health check protocol engine
- [x] Notification channels (Slack, Telegram, PagerDuty, webhooks)
- [ ] Connector ecosystem (Datadog, pganalyze, CloudWatch, Supabase)
- [ ] Issue tracker integration (GitHub, GitLab, Jira)

---

## Prior Art & Inspiration

| Tool | What We Take | What We Add |
|------|-------------|-------------|
| `psql` | Command set, muscle memory, wire protocol | Everything else |
| `pgcli` | Autocomplete, highlighting, named queries, destructive warnings | Rust performance, AI, TUI pager, diagnostics |
| `pspg` | Pager UX, column freeze | Integrated, not external |
| `postgres_dba` | Diagnostic queries | Built-in, not separate SQL files |
| `warp` | AI in terminal, status bar | Postgres-specific, not generic |

---

## License

Apache 2.0
