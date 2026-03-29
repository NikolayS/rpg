# AI test: postgres_dba integration

Tests that [postgres_dba](https://github.com/NikolayS/postgres_dba) interactive menu
works correctly inside rpg — enter menu, navigate a submenu, return, and quit cleanly.

## Prerequisites

- Docker container `samo-test-pg2` running on port 15433
- Database `demo` exists
- Binary built: `cargo build` in repo root
- `postgres_dba` cloned to `/tmp/postgres_dba`
- Terminal: 120×35, Dracula theme, font-size 13

## Setup

Clone postgres_dba (once):

```bash
git clone --depth=1 https://github.com/NikolayS/postgres_dba.git /tmp/postgres_dba
```

## Test steps

1. **Connect** — `rpg -U postgres -h demo -p 15433 demo`, wait for prompt

2. **Load menu** — `\i /tmp/postgres_dba/start.psql`
   - Menu renders with all options (0, 1, 2, 3, a1–v2, q)
   - `\prompt` fires, cursor waits for input

3. **Choose option 1** (Databases: size, stats) — type `1`, press Enter
   - SQL runs, table of databases + sizes appears
   - "Press <Enter> to continue…" prompt appears

4. **Continue** — press Enter
   - Menu re-renders (recursive `\ir ./start.psql`)
   - Cursor again waiting for input

5. **Quit** — type `q`, press Enter
   - "Bye!" prints
   - Returns to `demo=#` prompt cleanly

## Pass criteria

- Menu renders without error after `\i start.psql`
- `\prompt` accepts input in interactive mode (raw mode active)
- Choice `1` runs the databases SQL and displays results
- "Press <Enter> to continue…" prompt works (second `\prompt`)
- Recursive `\ir ./start.psql` re-renders the menu correctly
- `q` exits with "Bye!" — no hang, no crash, returns to rpg prompt
- No spurious errors in output

## Recording

Script: `tests/ai/postgres-dba.exp`
Output: `demos/postgres-dba.cast` → `demos/postgres-dba.gif`
Dimensions: 120×35

```bash
expect tests/ai/postgres-dba.exp
~/.cargo/bin/agg --theme dracula --font-size 13 \
    demos/postgres-dba.cast demos/postgres-dba.gif
```
