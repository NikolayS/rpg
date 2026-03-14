# CLAUDE.md — Rpg

## Project

Rpg — self-driving Postgres agent and psql-compatible terminal. Private repo: NikolayS/project-alpha.

## Style rules

Follow the shared rules at https://gitlab.com/postgres-ai/rules/-/tree/main/rules — key rules summarized below.

### SQL style (development__db-sql-style-guide)

- Lowercase SQL keywords — `select`, `from`, `where`, not `SELECT`, `FROM`, `WHERE`
- `snake_case` for all identifiers
- Root keywords on their own line; arguments indented below
- `AND`/`OR` at the beginning of the line
- Always use `AS` for aliases; use meaningful alias names
- Use CTEs over nested subqueries
- Functions as identifiers: `date_trunc()`, not `DATE_TRUNC()`
- ISO 8601 dates: `yyyy-mm-ddThh:mm:ss`
- Plural table names (`users`, `blog_posts`), `_id` suffix for FKs

```sql
-- Correct
select
    t.client_id as client_id,
    date(t.created_at) as day
from telemetry as t
inner join users as u
    on t.user_id = u.id
where
    t.submission_date > '2019-07-01'
    and t.sample_id = '10'
group by
    t.client_id,
    day;
```

### DB schema design (development__db-schema-design-guide)

- Primary keys: `int8 generated always as identity`
- Prefer `timestamptz` over `timestamp`, `text` over `varchar`
- Store money as cents (`int8`), never use `money` type
- Always add `comment on table` / `comment on column`
- Lowercase keywords, proper spacing

### Shell style (development__shell-style-guide)

Every script must start with:

```bash
#!/usr/bin/env bash
set -Eeuo pipefail
IFS=$'\n\t'
```

- 2-space indent, no tabs
- 80 char line limit
- Quote all variable expansions; prefer `${var}` over `$var`
- `[[ ]]` over `[ ]`; `$(command)` over backticks
- Errors to STDERR; use `trap cleanup EXIT`
- `lower_case` functions and variables; `UPPER_CASE` for constants
- Scripts with functions must have `main()` at bottom, last line: `main "$@"`

### PostgreSQL command execution (development__postgres-command-execution)

- Always use `--no-psqlrc` and `PAGER=cat`
- Prefer long options, one per line with `\` continuation
- Use `timeout` with `kubectl exec` to prevent hanging
- Avoid `-it` flags for non-interactive queries

```bash
timeout 10 kubectl exec pod-name -n namespace -- \
  env PAGER=cat psql \
    --no-psqlrc \
    --username=postgres \
    --dbname=mydb \
    --command="select version()"
```

### Git commits (development__git-commit-standards)

- Conventional Commits: `feat:`, `fix:`, `docs:`, `ops:`, `refactor:`, `chore:`, etc.
- Scope encouraged: `feat(auth): add OAuth`
- Subject < 50 chars, body lines < 72 chars
- Present tense ("add" not "added")
- Never amend — create new commits
- Never force-push unless explicitly confirmed

### Units and timestamps

- Binary units in docs/reports: GiB, MiB, KiB (not GB, MB, KB)
- Exception: PostgreSQL config values use PG format (`shared_buffers = '32GB'`)
- Dynamic UI: relative timestamps with ISO 8601 hover tooltip
- Static content: absolute timestamps `YYYY-MM-DD HH:mm:ss UTC`

## Architecture

See `SPEC.md` for the full specification. Key concepts:

- **AAA Architecture** — Analyzer / Actor / Auditor (triangle, not pipeline)
- **O/S/A Autonomy** — Observe / Supervised / Auto (per-feature)
- **Evidence Classification** — Factual / Heuristic / Advisory
- **Language:** Rust
- **Wire protocol:** tokio-postgres
- **PG support:** 14-18
