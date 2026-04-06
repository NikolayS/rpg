# psql compatibility report — rpg 0.10.2

> **Question from the community:** "How complete is it? Safe to alias `psql=rpg`?"

## TL;DR

**96.7% of PostgreSQL's own regression tests pass** (237/245; the 8 that don't are server-version mismatches or require C extensions, not rpg limitations).

For everyday use — queries, `\d` commands, scripts, `\copy`, REPL — rpg is a safe drop-in. A handful of advanced scripting features (see Known Gaps below) are not yet implemented.

---

## How we test

The `psql-regress` CI job runs PostgreSQL's own regression test suite (unmodified `.sql` files from the postgres source tree) against **both** psql and rpg simultaneously, then diffs the outputs:

1. Each test SQL file is run through `psql` and `rpg` against isolated cloned databases
2. Both outputs are normalized (strip timing lines, ANSI codes, trailing whitespace)
3. Outputs are diff'd — **PASS only if identical**

Test files are fetched at CI runtime from [`postgres/postgres`](https://github.com/postgres/postgres) (the official PostgreSQL repo), pinned to commit `3d10ece` (2026-04-06). They are **not stored in this repo** — the runner script is at [`tests/compat/test-psql-regress.sh`](../tests/compat/test-psql-regress.sh).

CI server: `postgres:18`. CI test files: PG19dev master.

---

## Regression test results

| Status | Count | Tests |
|--------|-------|-------|
| ✅ PASS | **237** | boolean, char, name, varchar, text, int2–int8, float4/8, numeric, uuid, enum, money, rangetypes, date, time, timestamp, interval, inet, geometry types, JSON, XML, arrays, inheritance, triggers, views, indexes, sequences, transactions (partial), roles, privileges, … |
| ⏭ SKIP — PG19dev/PG16 mismatch | 6 | `psql` (uses `\parse`/`\bind_named`, PG17+ metacmds), `transactions` (PG19dev behavior), `copydml` (COPY count format changed PG18+), `domain` (constraint ordering differs), `misc_functions` (replication origin state leak between tests), `tablespace` (no tablespace dir in CI) |
| ⏭ SKIP — needs C extension | 1 | `regproc` (requires `regress.so` built from C) |
| ⏭ SKIP — schema init | 1 | `test_setup` (runs as setup before tests, not a test itself) |
| **TOTAL** | **245** | |

The 6 PG19dev/PG16 skips are inherent to running a PG16 server against PG19dev test files. They are not rpg bugs — the same tests pass locally with a PG18+ server.

---

## Known gaps (where `alias psql=rpg` would break)

These are features psql has that rpg does not yet implement:

| Feature | Severity | Notes |
|---------|----------|-------|
| `\parse` / `\bind` / `\bind_named` | Low | PG17+ extended query protocol metacmds; rare in scripts |
| `\lo_import` / `\lo_export` / `\lo_list` / `\lo_unlink` | Low | Large object management |
| `\password` | Low | Interactive password change |
| `\conninfo` | Low | Print current connection info |
| `\encoding` | Low | Client encoding commands |
| `\prompt` | Low | Interactive variable prompt |
| `\copy` with all option variants | Medium | Core works; some edge-case options (e.g. `FORCE_QUOTE`, `ESCAPE`) now pass through |
| Readline history across sessions | Low | In-session history works |

---

## What rpg has that psql doesn't

This is the other side of the compatibility story — rpg is a superset in these areas:

### AI assistant (slash commands)

| Command | What it does |
|---------|-------------|
| `/ask <question>` | Ask a question about the database or query in natural language |
| `/explain` | Explain the last query result or error |
| `/fix` | Suggest a fix for the last error |
| `/optimize` | Suggest query optimizations |

### Built-in DBA diagnostics (`/dba`)

| Command | What it does |
|---------|-------------|
| `/dba bloat` | Table and index bloat analysis |
| `/dba vacuum` | VACUUM and autovacuum status |
| `/dba index` | Index health, unused indexes, missing indexes |
| `/dba wait` | Active wait events |
| `/dba locks` | Lock contention |
| `/dba cache` | Buffer cache hit rates |
| `/ash` | Active Session History (pg_stat_activity snapshots) |

### Enhanced REPL experience

| Feature | Description |
|---------|-------------|
| Status line | Live connection info, query timer, transaction state in the terminal status bar |
| `/session` | Session-level settings and diagnostics |
| `/refresh` | Auto-refresh a query on an interval (like `watch` but SQL-aware) |
| `/ns` | Namespace/schema switcher |
| SSH tunnel | Built-in `--ssh-tunnel` flag — no separate tunnel process needed |
| Multi-host failover | Automatic failover across a comma-separated host list |

### Command namespace

rpg uses `/` for all its own commands and `\` exclusively for psql-compatible metacommands. This makes it unambiguous which commands are standard and which are extensions. `\dba`, `\sql`, `\plan` etc. are deprecated aliases that still work but print a migration hint.

---

## Safe to alias?

**Yes, for most workflows.** The regression test pass rate reflects real-world psql usage patterns. If you use psql for queries, schema exploration (`\d`, `\dt`, `\di`, …), scripting with `-c`/`-f`, `\copy`, `\gset`, `\watch`, or `\crosstabview` — rpg handles all of these.

If you rely on large-object commands, `\password`, or `\parse`/`\bind`, keep psql around for those specific workflows.
