# AI test: quickstart-demo

Hero demo for the top of README — shows rpg's key features in one continuous flow:
connect, introspect, error recovery with `/fix`, and live wait event monitoring with `/ash`.

## Prerequisites

- Docker container `samo-test-pg2` running on port 15433
- Database `demodb` exists with demo schema (customers, orders, products)
- Database `ashtest` running pgbench load (for `/ash` activity)
- Demo DB setup: `psql postgresql://postgres@127.0.0.1:15433/postgres -f tests/ai/quickstart-demo-setup.sql`
- Binary built: `cargo build` in repo root
- Terminal: 120×35, Dracula theme, font-size 13

## Setup

Initialize pgbench schema (once):

```bash
pgbench -i -s 1 postgresql://postgres@127.0.0.1:15433/ashtest
```

Start background load for `/ash` segment (before recording):

```bash
pgbench -c 8 -T 120 postgresql://postgres@127.0.0.1:15433/ashtest &
BENCH_PID=$!
```

## Test steps

1. **Open rpg** — type `rpg <connstr>` with human-like speed, wait for prompt:
   - Status bar appears: `ashtest | postgres | ...`
   - Default prompt `ashtest=#`

2. **Check version** — type `/version` at the prompt:
   - Output shows `rpg vX.Y.Z`

3. **List tables** — type `\d` with brief pause after backslash:
   - Output shows table list (pgbench tables: `pgbench_accounts`, `pgbench_branches`,
     `pgbench_history`, `pgbench_tellers`)
   - Column headers, row count shown

4. **Intentional typo query** — type `select * from orderss limit 1;` slowly:
   - rpg shows SQL error: `ERROR: relation "orderss" does not exist`
   - Hint line: `type /fix to auto-correct this query`

5. **Auto-fix** — type `/fix`:
   - AI detects the typo (`orderss` → nearest real table)
   - Corrected SQL shown in preview box
   - Execute? prompt appears — type `n` (decline, we just want to show the fix)

6. **Open `/ash`** — type `/ash`:
   - Full-screen TUI opens
   - Status bar: `/ash  [Live]  interval: 1s  window: Ns  active: N`
   - Bars accumulate and scroll left (pgbench load is running)
   - Observe for ~8 seconds

7. **Drill down** — press `↓` to select top wait type, then `Enter`:
   - Drill-down panel shows per-event breakdown

8. **Exit** — press `q` to exit `/ash`, back to rpg prompt

## Pass criteria

- Typing is human-like (not instant), with 0.7s pause after each command before next input
- rpg prompt appears cleanly after connect
- `/version` shows version string
- `\d` shows pgbench table list
- SQL typo triggers error + `/fix` hint
- `/fix` shows corrected query in preview box
- `/ash` TUI renders with colored bars from pgbench load
- Drill-down works with `↓` + `Enter`
- Clean exit on `q`

## Recording

Script: `tests/ai/quickstart-demo.exp`
Output: `demos/quickstart-demo.cast` → `demos/quickstart-demo.gif`
Dimensions: 120×35

```bash
# Full record + convert pipeline
pgbench -c 8 -T 120 postgresql://postgres@127.0.0.1:15433/ashtest &
BENCH_PID=$!
sleep 2   # let load build up
expect tests/ai/quickstart-demo.exp
~/.cargo/bin/agg --theme dracula --font-size 13 \
    demos/quickstart-demo.cast demos/quickstart-demo.gif
kill $BENCH_PID
```
