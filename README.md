# rpg — self-driving Postgres agent

A psql-compatible terminal with built-in DBA diagnostics and AI assistant. Single binary, no dependencies, cross-platform.

## Quick start

```bash
# Connect like psql
rpg -h localhost -p 5432 -U postgres -d mydb

# Or use a connection string
rpg "postgresql://user@localhost/mydb"

# Run a query non-interactively
rpg -d postgres -c "select version()"
```

## DBA diagnostics

rpg ships 15+ diagnostic commands accessible via `\dba`:

```
postgres=# \dba
\dba diagnostic commands:
  \dba activity    pg_stat_activity: grouped by state, duration, wait events
  \dba locks       Lock tree (blocked/blocking)
  \dba waits       Wait event breakdown (+ for AI interpretation)
  \dba bloat       Table bloat estimates
  \dba vacuum      Vacuum status and dead tuples
  \dba tablesize   Largest tables
  \dba connections Connection counts by state
  \dba indexes     Index health report (unused, redundant, invalid, bloated)
  \dba unused-idx  Unused indexes (simple view)
  \dba seq-scans   Tables with high sequential scan ratio
  \dba cache-hit   Buffer cache hit ratios
  \dba replication Replication slot status
  \dba config      Non-default configuration parameters
  \dba progress    Long-running operation progress (pg_stat_progress_*)
  \dba io          I/O statistics by backend type (PG 16+, verbose: \dba+ io)
```

### Index health

Scans for unused, redundant, invalid, and bloated indexes with actionable suggestions:

```
postgres=# \dba indexes
Index health: 4 issues found.

!  [unused] public.orders
   index: orders_status_idx
   Index orders_status_idx has 0 scans since stats reset (unknown)
   suggestion: DROP INDEX CONCURRENTLY public.orders_status_idx
   size: 16.0 KB

!  [unused] public.orders
   index: orders_created_at_idx
   Index orders_created_at_idx has 0 scans since stats reset (unknown)
   suggestion: DROP INDEX CONCURRENTLY public.orders_created_at_idx
   size: 16.0 KB
```

### Activity overview

```
postgres=# \dba activity
  pid  | user |  database  | state | wait_event_type |   wait_event   |   backend_type   | duration |           query
-------+------+------------+-------+-----------------+----------------+------------------+----------+----------------------------
 47936 | nik  | postgres   | idle  | Client          | ClientRead     | client backend   |    8617s | select count(*) from users;
  1070 |      |            |       | Activity        | AutovacuumMain | autovacuum launcher |       |
(...)
0 active, 0 idle in transaction, 2 idle, 8 total
```

### Cache hit ratios

```
postgres=# \dba cache-hit
 schemaname |    relname    | heap_blks_hit | heap_blks_read | hit_pct
------------+---------------+---------------+----------------+---------
 public     | bloat_test_50 | 339229        | 774            | 99.77
 public     | users         | 110           | 0              | 100.00
 public     | orders        | 74            | 0              | 100.00
 public     | products      | 39            | 0              | 100.00
(4 rows)
```

### Vacuum status

```
postgres=# \dba vacuum
 schemaname |    relname    | n_live_tup | n_dead_tup | last_autovacuum               | vacuum_count | autovacuum_count | xid_age
------------+---------------+------------+------------+-------------------------------+--------------+------------------+---------
 public     | bloat_test_50 | 75000      | 0          | 2026-03-13 20:46:08.983068-07 | 0            | 1                | 1
 public     | users         | 10         | 0          |                               | 0            | 0                | 65
 public     | orders        | 24         | 0          |                               | 0            | 0                | 53
(...)
```

## AI assistant

rpg integrates with OpenAI, Anthropic, and Ollama for AI-powered Postgres assistance:

```
-- Ask questions about your database
postgres=# /ask What indexes should I add for my orders table?

-- Get explanations of query plans
postgres=# explain select * from orders where status = 'pending';
postgres=# /explain

-- Fix errors automatically
postgres=# /fix

-- Optimize slow queries
postgres=# /optimize
```

Configure with environment variables or in-session:

```
postgres=# \set AI_PROVIDER anthropic
postgres=# \set AI_MODEL claude-sonnet-4-20250514
```

## Features

- **psql-compatible** — drop-in replacement for daily psql usage (`\d`, `\dt`, `\di`, `\l`, `\du`, `\sf`, `\sv`, `\copy`, ...)
- **DBA diagnostics** — 15+ `\dba` commands for activity, locks, bloat, vacuum, indexes, cache, replication, I/O
- **AI assistant** — `/ask`, `/fix`, `/explain`, `/optimize` with OpenAI, Anthropic, or Ollama
- **Syntax highlighting** — SQL keywords, strings, numbers, and schema objects
- **Schema-aware completion** — tab completion for tables, columns, functions, keywords
- **TUI pager** — built-in scrollable pager (like `less`) for large result sets
- **Named queries** — save and recall frequently used queries
- **SSH tunneling** — built-in SSH tunnel support for remote databases
- **Session persistence** — query history and settings preserved across sessions
- **Config profiles** — per-project configuration via `.rpg.toml`
- **Daemon mode** — background monitoring with anomaly detection and Slack alerts
- **Status bar** — persistent bottom bar showing connection info, transaction state, timing
- **Cross-platform** — single static binary for Linux, macOS, Windows (x86_64 + aarch64)

## PostgreSQL compatibility

Supports PostgreSQL 14, 15, 16, 17, and 18.

## Building from source

Requires Rust 1.85+.

```bash
cargo build --release
./target/release/rpg --help
```

## License

Apache 2.0 — see [LICENSE](LICENSE).
