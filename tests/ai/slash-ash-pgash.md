# AI test: slash-ash-pgash

Tests `/ash` with `pg_ash` installed — verifies that the timeline is pre-populated
with historical data from `ash.wait_timeline()` before live polling begins.

## Prerequisites

- pg_ash v1.2+ installed on the test Postgres server
- Binary built: `cargo build` in repo root
- pgbench available

## Setup

Install pg_ash and generate at least 5 minutes of history:

```bash
# Install pg_ash (requires superuser)
psql postgresql://postgres@127.0.0.1:15433/ashtest -c "CREATE EXTENSION IF NOT EXISTS pg_ash;"

# Verify ash.wait_timeline exists
psql postgresql://postgres@127.0.0.1:15433/ashtest -c "\df ash.wait_timeline"

# Initialize pgbench schema
pgbench -i -s 5 postgresql://postgres@127.0.0.1:15433/ashtest

# Run load for 5 minutes to populate ash.samples
pgbench -c 8 -T 300 postgresql://postgres@127.0.0.1:15433/ashtest &
BENCH_PID=$!
echo "pgbench running as PID $BENCH_PID — waiting 5 minutes..."
sleep 300
kill $BENCH_PID 2>/dev/null
echo "Load generation complete."

# Verify history exists
psql postgresql://postgres@127.0.0.1:15433/ashtest \
  -c "SELECT count(*) FROM ash.wait_timeline('10 minutes'::interval, '1 second'::interval);"
# Should return > 0 rows
```

## Test steps

1. Connect to ashtest:
   ```
   ./target/debug/rpg postgresql://postgres@127.0.0.1:15433/ashtest
   ```

2. Run `/ash` — verify immediate history pre-population:
   - Timeline bars appear from the LEFT side immediately (not just the right)
   - Bars cover most of the timeline width without waiting for live data
   - AAS value reflects historical load (should be > 0 given pgbench ran)
   - Status bar shows `/ash  [Live]  active: N`

3. Verify color correctness:
   - CPU* (bright green) should dominate from pgbench workload
   - IO (blue) visible during pgbench table scans
   - Colors stable, no position jumping

4. Wait 10 seconds — verify live data appends on right:
   - New bars appear on the far right
   - Historical bars shift left (scrolling behavior)
   - AAS updates each second

5. Press `←` to zoom out — verify deeper history visible:
   - More buckets shown, covering longer time span
   - Historical data fills more of the timeline

6. Press `q` — verify clean exit

7. Cleanup:
   ```bash
   # Optionally drop pg_ash if not needed
   psql postgresql://postgres@127.0.0.1:15433/ashtest -c "DROP EXTENSION pg_ash;"
   ```

## Fallback test (pg_ash NOT installed)

Verify graceful degradation when pg_ash is absent:

1. Connect to a DB without pg_ash (e.g. port 15434)
2. Run `/ash`
3. Verify: timeline starts empty and fills from right as live data arrives
4. Verify: no error message, no crash

## Recording demo GIF

Requires pg_ash pre-loaded with 5+ minutes of history (see Setup above).

```bash
# Start background load for live portion
pgbench -c 4 -T 120 postgresql://postgres@127.0.0.1:15433/ashtest &
LIVE_PID=$!

# Record
expect tests/ai/slash-ash-pgash.exp

kill $LIVE_PID 2>/dev/null

# Convert
agg --theme dracula --font-size 13 demos/slash-ash-pgash.cast demos/slash-ash-pgash.gif

# Resize if needed
convert demos/slash-ash-pgash.gif -coalesce -resize 800x -layers optimize demos/slash-ash-pgash.gif

# Commit
git add demos/slash-ash-pgash.gif demos/slash-ash-pgash.cast
git commit -m "docs(ash): add pg_ash history integration demo GIF"
```

Expect script (`tests/ai/slash-ash-pgash.exp`):

```
#!/usr/bin/expect -f
set timeout 60
spawn env TERM=xterm-256color COLORTERM=truecolor COLUMNS=120 LINES=35 \
    asciinema rec demos/slash-ash-pgash.cast --overwrite --cols 120 --rows 35
expect -re {[$]\s*$} { sleep 0.5 }
send "./target/debug/rpg postgresql://postgres@127.0.0.1:15433/ashtest\r"
expect -re {[#>]\s*$} { sleep 1 }
send "/ash\r"
sleep 1
sleep 25
send "q"
sleep 2
send "\\q\r"
sleep 1
send "\x04"
sleep 2
```

## Pass criteria

- Timeline pre-populated with historical bars immediately on `/ash` launch
- Left side of timeline shows history (not empty)
- Colors match pg_ash scheme (CPU* bright green dominant under pgbench load)
- Live data appends to the right after initial population
- Graceful fallback to live-only when pg_ash not installed (no crash)
- GIF shows non-empty timeline from frame 1
