# Samo

> The best terminal for diagnosing and fixing Postgres production issues.

**Status:** Vision / pre-development  
**Language:** Rust  
**Org:** [PostgresAI](https://postgres.ai)

---

## What is this?

A self-driving Postgres agent that lives in your terminal. Four things that have never been combined:

1. **An autonomous agent** — detects, diagnoses, and resolves database issues with per-feature autonomy levels and the AAA Architecture (Analyzer/Actor/Auditor)
2. **An AI-powered terminal** — LLM inside, understands your schema, explains errors, writes and optimizes SQL, performs root cause analysis
3. **Batteries included** — pgcli-style autocomplete, pspg-style pager, postgres_dba diagnostics built in
4. **psql-compatible** — common psql workflows work out of the box, respecting 30 years of muscle memory

Think: `self-driving postgres` at the core, `pgcli` for UX, `warp` for AI, `openclaw` for connectivity.

---

## Why?

Every production Postgres incident follows the same painful loop: notice something's wrong, SSH in, run ad-hoc queries against pg_stat_activity, reconstruct what happened, figure out a fix, apply it, hope it works. This takes 30-60 minutes even for experienced DBAs.

What if your terminal could do that investigation in seconds, propose the fix, and — with your permission — apply it?

AI coding tools (Cursor, Warp, Claude Code) proved that putting an LLM *inside* the tool you already use is transformative. Nobody's done this for Postgres.

**The opportunity:** Build the terminal where Postgres expertise meets autonomous AI operations. The psql compatibility gets you in the door; the self-driving capabilities keep you there.

---

## Architecture

```
┌─────────────────────────────────────────────────────┐
│                       samo                          │
│                                                     │
│  ┌──────────────────────────────────────────────┐   │
│  │      Autonomous Agent — AAA Architecture     │   │
│  │                                              │   │
│  │  ┌──────────┐ ┌──────────┐ ┌──────────┐     │   │
│  │  │ ANALYZER │→│  ACTOR   │→│ AUDITOR  │     │   │
│  │  │ (thinks) │ │(executes)│ │(verifies)│     │   │
│  │  └──────────┘ └──────────┘ └──────────┘     │   │
│  │                                              │   │
│  │  Per-feature autonomy (A/G/P × 18 areas)    │   │
│  │  RCA • Index Health • Vacuum • Config • ...  │   │
│  │  Connectors: pg_ash │ CloudWatch/RDS         │   │
│  │  Modes: interactive │ daemon                 │   │
│  └──────────────────────────────────────────────┘   │
│                                                     │
│  ┌─────────────┐  ┌──────────────┐  ┌───────────┐  │
│  │  AI Engine  │  │  UX Layer    │  │  Core     │  │
│  │             │  │              │  │           │  │
│  │ LLM engine  │  │ Autocomplete │  │ Wire proto│  │
│  │ NL → SQL    │  │ Highlighting │  │ \commands │  │
│  │ EXPLAIN     │  │ TUI pager    │  │ COPY/LOB  │  │
│  │ RCA chain   │  │ \dba diags   │  │ .psqlrc   │  │
│  │ /ask        │  │              │  │ Formatting│  │
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

### Rust Foundations
- `tokio-postgres` — async wire protocol
- `rustyline` — readline with history, completion
- `clap` — CLI argument parsing

---

## Layer 2: Batteries Included

Everything `pgcli` and `pspg` do, built in.

### Schema-Aware Autocomplete
- Queries `pg_catalog` on connect to build completion tree
- Table/column/function/type names contextual to the query being written
- Keyword completion with Postgres version awareness

### Syntax Highlighting
- Real-time SQL highlighting in the input line
- `tree-sitter-sql` or `syntect` for parsing

### Integrated TUI Pager
- Replaces the need for an external pager (less/pspg)
- Column freezing, horizontal scroll, search
- Built with `ratatui`
- Can still pipe to external pager if preferred

### Built-in Diagnostics (postgres_dba)
[postgres_dba](https://github.com/NikolayS/postgres_dba) queries available as first-class commands:

```
\dba activity     — current activity (pg_stat_activity)
\dba bloat        — table and index bloat
\dba locks        — lock trees and conflicts
\dba unused-idx   — unused indexes
\dba seq-scans    — tables with excessive seq scans
\dba cache-hit    — buffer cache hit ratio
\dba vacuum       — vacuum/autovacuum status
\dba replication  — replication lag and status
```

---

## Layer 3: AI Terminal

An LLM lives inside the terminal.

### Capabilities
- **Natural language → SQL** — "show me the 10 biggest tables" → generates and optionally runs the query
- **Error explanation** — failed query? Get a human-readable diagnosis with fix suggestions
- **EXPLAIN analysis** — paste or run `EXPLAIN ANALYZE`, get plain-English breakdown with optimization suggestions
- **Schema-aware context** — the LLM knows your tables, columns, indexes, constraints
- **Query optimization** — suggest rewrites, missing indexes, better join strategies
- **pg_ash integration** — feed wait event data to LLM for workload analysis

### Interface
```
-- Natural language mode
/ask show me the top 10 queries by total time from pg_ash

-- Inline explanation
samo=> SELECT * FROM orders WHERE status = 'pending';
ERROR: column "status" does not exist
-- 💡 Did you mean "order_status"? (orders.order_status text NOT NULL)

-- EXPLAIN analysis
samo=> /explain SELECT * FROM orders JOIN customers ON ...
-- Returns annotated plan with bottleneck identification
```

### LLM Backend
- Pluggable: OpenAI, Anthropic, local models (ollama)
- Context window management — schema + recent queries + pg_ash data as context
- Streaming responses in terminal

---

## Autonomous Agent

The core differentiator — not just a terminal, but a self-driving Postgres agent.

### Per-Feature Autonomy (3 levels × N feature areas)

Each feature area (index health, vacuum, config tuning, upgrades, etc.) is independently configured:

| Level | Name | What it means |
|-------|------|---------------|
| **O** | **Observe** | Read-only. Observe, diagnose, report. Zero writes. |
| **S** | **Supervised** | Act with human approval. Proposes action, human confirms. |
| **A** | **Auto** | Act autonomously within policy and DB permissions. Human notified after. |

The **AAA Architecture** (Analyzer → Actor → Auditor) — three isolated governance components ensure the decision-maker never has direct execution access.

### What the Agent Does
- **Root cause analysis** — reconstruct block trees, correlate pg_ash wait events with metrics and locks, produce structured RCA reports with three-tier mitigation (immediate/mid-term/long-term)
- **Continuous health monitoring** — detect anomalies, session spikes, lock cascades, bloat, stale stats
- **Auto-remediation** — cancel blockers, reindex, vacuum, tune GUCs (within autonomy level and DB permissions)
- **Issue tracking** — create/update issues with full RCA evidence
- **Escalation** — when something exceeds autonomy level, create a detailed ticket or alert

### Connectors

| Source | What We Get |
|--------|-------------|
| **pg_ash** | Wait events, query-level performance, active session history |
| **PostgresAI Monitoring & Checkup** | Historical metrics, health scores, checkup reports, baselines |
| **PostgresAI Issues** | Issue tracking, RCA linkage, remediation status, evidence |
| **Datadog** | Infrastructure metrics, custom monitors, dashboards |
| **pganalyze** | Query statistics, EXPLAIN plans, index advisor suggestions |
| **AWS CloudWatch** | CloudWatch metrics/logs/alarms, RDS Performance Insights, Enhanced Monitoring |
| **Supabase** | Project management API, Postgres via pooler |

### Modes

**Interactive** — human at the terminal, agent assists and suggests in real-time.

**Daemon/Container** — runs headless, follows protocols, reports via configured channels (Slack, email, GitHub issues). Deployable as a sidecar container next to your Postgres.

```bash
# Interactive — agent assists in real-time
samo --host prod-db-01 --autonomy rca:supervised,index_health:observe

# Daemon mode — headless monitoring and remediation
samo daemon --config /etc/samo/config.toml
```

---

## Roadmap

### Phase 0: Foundation (Month 1-2)
- [ ] Wire protocol client with auth (SCRAM, SSL, password)
- [ ] Basic REPL with rustyline
- [ ] Core backslash commands (\d, \dt, \di, \l, \c, \x, \timing)
- [ ] Output formatting (aligned, \x expanded)
- [ ] Basic autocomplete (keywords + schema objects)
- [ ] CI with cross-compilation (Linux, macOS, both architectures)

### Phase 1: Daily Driver (Month 2-4)
- [ ] Remaining common backslash commands (\copy, \e, \i, \set, \watch)
- [ ] Syntax highlighting
- [ ] TUI pager (ratatui)
- [ ] postgres_dba diagnostics as \dba commands
- [ ] .psqlrc basic support
- [ ] Single binary distribution

### Phase 2: AI Brain (Month 3-5)
- [ ] LLM integration framework (pluggable providers)
- [ ] /ask command — NL → SQL
- [ ] Error explanation with schema context
- [ ] EXPLAIN ANALYZE interpreter
- [ ] pg_ash query and visualization
- [ ] Context management (schema + session + history)

### Phase 3: Agent (Month 4-7)
- [ ] Autonomy level framework
- [ ] Health check protocol engine
- [ ] First connectors (pg_ash native)
- [ ] Daemon mode
- [ ] Issue tracker integration (GitHub)
- [ ] Alert/notification channels

### Phase 4: Ecosystem (Month 6+)
- [ ] Additional connectors (Datadog, pganalyze, RDS, Supabase)
- [ ] Jira/GitLab integration
- [ ] Plugin system for custom connectors
- [ ] Container/sidecar packaging
- [ ] Protocol marketplace (community health check playbooks)

---

## Rust Crate Dependencies (Initial)

| Crate | Purpose |
|-------|---------|
| `tokio-postgres` | Wire protocol, async queries |
| `rustls` / `native-tls` | SSL connections |
| `rustyline` | REPL, history, completion |
| `ratatui` + `crossterm` | TUI pager |
| `syntect` or `tree-sitter` | Syntax highlighting |
| `clap` | CLI argument parsing |
| `serde` + `toml` | Config files |
| `reqwest` | HTTP for API connectors |
| `tracing` | Structured logging |
| `tokio` | Async runtime |

---

## Prior Art & Inspiration

| Tool | What We Take | What We Add |
|------|-------------|-------------|
| `psql` | Command set, muscle memory, wire protocol | Everything else |
| `pgcli` | Autocomplete, highlighting, named queries, destructive warnings, fuzzy match, vi/emacs modes | Rust performance, AI, agent, TUI pager |
| `pspg` | Pager UX, column freeze | Integrated, not external |
| `postgres_dba` | Diagnostic queries | Built-in, not separate SQL files |
| `warp` | AI in terminal, status bar | Postgres-specific, not generic |
| `claude-code` | Plan mode, sessions, compaction, undo, MCP, project files, subagents | Database-specific domain expertise |
| `opencode` | TUI, session persistence (SQLite), auto-compact, LSP-like intelligence | Postgres wire protocol as the "language server" |
| `openclaw` | Multi-session, memory, connectors, cron/heartbeats, channel delivery | Native Postgres agent, not generic AI |

---

## License

Apache 2.0

---

*This document is the vision. Implementation starts with Layer 1.*
