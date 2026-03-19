# Demo GIFs

This directory contains recorded terminal demos of rpg features, along with
the [VHS](https://github.com/charmbracelet/vhs) tape files used to render them.

| File | What it shows |
|------|---------------|
| `gif1_optimize.gif` | Slow query → `/explain` → `/optimize` → index creation → fast re-run |
| `gif2_typo.gif` | Typo in table name → `/fix` corrects and re-executes |
| `gif3_t2s.gif` | `\t2s` text-to-SQL with confirmation, then `\yolo` auto-execute |

## Prerequisites

- [VHS](https://github.com/charmbracelet/vhs) installed (`brew install vhs` on macOS)
- rpg built from source (see top-level README)
- PostgreSQL running locally with a `demo_saas` database

## Setting up the demo database

Create and populate the database using the provided SQL script:

```bash
createdb demo_saas
psql -d demo_saas -f demos/setup_demo_db.sql
```

See [setup_demo_db.sql](setup_demo_db.sql) for the full schema and data
generation queries.

## Rendering the GIFs

Make sure rpg is on your PATH (the tapes expect the debug build):

```bash
export PATH=/path/to/rpg/target/debug:$PATH
```

Render each GIF individually:

```bash
vhs demos/gif1_optimize.tape
vhs demos/gif2_typo.tape
vhs demos/gif3_t2s.tape
```

Or render all at once:

```bash
for tape in demos/*.tape; do vhs "$tape"; done
```

## Note on gif1 re-renders

`gif1_optimize.tape` creates an index on `orders (status, created_at desc)`
during the recording. Before re-rendering, drop that index so the slow-path
sequential scan is visible again:

```sql
drop index concurrently if exists orders_status_created_at_idx;
```

Then re-run the tape.

---

Copyright 2026 Postgres.ai
