# rpg — modern Postgres terminal

[![CI](https://github.com/NikolayS/rpg/actions/workflows/ci.yml/badge.svg)](https://github.com/NikolayS/rpg/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.82%2B-orange.svg)](https://www.rust-lang.org/)

A psql-compatible terminal with built-in DBA diagnostics and AI assistant.
Single binary, no dependencies, cross-platform.

## Installation

Build from source (requires Rust 1.85+):

```bash
git clone https://github.com/NikolayS/rpg.git
cd rpg
cargo build --release
sudo cp ./target/release/rpg /usr/local/bin/
```

## Connect

```bash
# Same flags as psql
rpg -h localhost -p 5432 -U postgres -d mydb

# Connection string
rpg "postgresql://user@localhost/mydb"

# Non-interactive
rpg -d postgres -c "select version()"
```

## DBA diagnostics

15+ diagnostic commands accessible via `\dba`:

```
postgres=# \dba
  \dba activity    pg_stat_activity: grouped by state, duration, wait events
  \dba locks       Lock tree (blocked/blocking)
  \dba waits       Wait event breakdown (+ for AI interpretation)
  \dba bloat       Table bloat estimates
  \dba vacuum      Vacuum status and dead tuples
  \dba tablesize   Largest tables
  \dba connections Connection counts by state
  \dba indexes     Index health (unused, redundant, invalid, bloated)
  \dba seq-scans   Tables with high sequential scan ratio
  \dba cache-hit   Buffer cache hit ratios
  \dba replication Replication slot status
  \dba config      Non-default configuration parameters
  \dba progress    Long-running operation progress (pg_stat_progress_*)
  \dba io          I/O statistics by backend type (PG 16+)
```

Index health example:

```
postgres=# \dba indexes
Index health: 2 issues found.

!  [unused] public.orders
   index: orders_status_idx  (0 scans since stats reset, 16 KiB)
   suggestion: DROP INDEX CONCURRENTLY public.orders_status_idx
```

## AI assistant

Integrates with OpenAI, Anthropic, and Ollama:

```sql
-- Ask questions about your database
/ask What indexes should I add for my orders table?

-- Interpret EXPLAIN ANALYZE output
explain select * from orders where status = 'pending';
/explain

-- Fix errors and optimize queries
/fix
/optimize
```

### /fix — auto-correct errors

```
postgres=# select * fromm t1 where i = 10;
ERROR:  syntax error at or near "fromm"
LINE 1: select * fromm t1 where i = 10;
                 ^
Hint: Replace "fromm" with "from".
Hint: type /fix to auto-correct this query

postgres=# /fix
Corrected SQL query:
┌── sql
select * from t1 where i = 10;
└───────
Execute? [Y/n/e]
  i |             random
----+--------------------
 10 | 0.6895257944299762
(1 row)
```

### /optimize — index and performance suggestions

```
postgres=# /optimize
<runs EXPLAIN (ANALYZE, BUFFERS), then suggests:>

1. Create an Index on t1.i — parallel seq scan is inefficient for point lookups
   CREATE INDEX idx_t1_i ON public.t1 (i);
   Expected: 28ms → sub-millisecond

2. Run ANALYZE on t1 — statistics may be stale
   ANALYZE public.t1;
```

## SSH tunnel

Connect through an SSH bastion with no extra tooling:

```bash
rpg --ssh-tunnel user@bastion.example.com -h 10.0.0.5 -d mydb
```

## Features

- **psql-compatible** — drop-in replacement (`\d`, `\dt`, `\copy`, `\watch`, ...)
- **DBA diagnostics** — 15+ `\dba` commands for activity, locks, bloat, indexes
- **AI assistant** — `/ask`, `/fix`, `/explain`, `/optimize`
- **Schema-aware completion** — tab completion for tables, columns, keywords
- **TUI pager** — scrollable pager for large result sets
- **Syntax highlighting** — SQL keywords, strings, operators; color-coded errors (red), warnings (yellow), notices (cyan)
- **Named queries** — save and recall frequent queries
- **Session persistence** — history and settings preserved across sessions
- **Config profiles** — per-project `.rpg.toml`
- **Status bar** — connection info, transaction state, timing
- **Cross-platform** — single static binary: Linux, macOS, Windows (x86_64 + aarch64)

## PostgreSQL compatibility

Supports PostgreSQL 14, 15, 16, 17, and 18.

## Development

rpg is engineered by [Nikolay Samokhvalov](https://github.com/NikolayS) with a
team of Claude Opus 4.6 AI agents (via [Claude Code](https://claude.com/claude-code),
occasionally with OpenClaw). All architecture decisions, feature design, and
project direction are human-driven. The codebase is large and 100% of the code
has been AI-reviewed and tested, though only a portion has been manually reviewed
line-by-line.

## License

Apache 2.0 — see [LICENSE](LICENSE).
