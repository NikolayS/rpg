# rpg — modern Postgres terminal

A psql-compatible terminal with built-in DBA diagnostics and AI assistant.
Single binary, no dependencies, cross-platform.

## Installation

### Homebrew (macOS / Linux)

```bash
brew tap NikolayS/rpg
brew install rpg
```

### Install script (macOS / Linux)

```bash
curl -fsSL https://raw.githubusercontent.com/NikolayS/project-alpha/main/scripts/install.sh | bash
```

### Build from source

Requires Rust 1.85+.

```bash
git clone https://github.com/NikolayS/project-alpha.git
cd project-alpha
cargo build --release
cp ./target/release/rpg /usr/local/bin/
```

### Self-update

```bash
rpg --update
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

```
postgres=# \set AI_PROVIDER anthropic
postgres=# \set AI_MODEL claude-sonnet-4-20250514
```

## Health checks and reports

```bash
rpg --check    # run all health checks and exit
rpg --report   # generate a full diagnostic report
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
- **pgcli-style completion** — dropdown with arrow navigation, schema-aware
- **TUI pager** — scrollable pager for large result sets
- **Syntax highlighting** — SQL keywords, strings, schema objects
- **Named queries** — save and recall frequent queries
- **Session persistence** — history and settings preserved across sessions
- **Config profiles** — per-project `.rpg.toml`
- **Status bar** — connection info, transaction state, timing
- **Cross-platform** — single static binary: Linux, macOS, Windows (x86_64 + aarch64)

## PostgreSQL compatibility

Supports PostgreSQL 14, 15, 16, 17, and 18.

## License

Apache 2.0 — see [LICENSE](LICENSE).
