# AI test: slash-ash-general

Tests the `/ash` Active Session History TUI — basic functionality, visual correctness,
drill-down navigation, and demo recording.

## Prerequisites

- Docker container `samo-test-pg2` running on port 15433
- Database `ashtest` exists (created via `createdb ashtest`)
- Binary built: `cargo build` in repo root

## Setup

Generate background load so ASH has sessions to display:

```bash
pgbench -i -s 1 postgresql://postgres@127.0.0.1:15433/ashtest
pgbench -c 8 -T 60 postgresql://postgres@127.0.0.1:15433/ashtest &
BENCH_PID=$!
```

## Test steps

1. Connect to ashtest:
   ```
   ./target/debug/rpg postgresql://postgres@127.0.0.1:15433/ashtest
   ```

2. Run `/ash` — verify initial render:
   - Status bar shows: `/ash  [Live]  interval: 1s  window: Ns  active: N`
   - Timeline block has visible border, title shows zoom level and AAS value
   - Y-axis labels are readable (not invisible against background)
   - Bars appear on the right side and grow left over time

3. Wait 10+ seconds — verify bars accumulate correctly:
   - Colors match pg_ash scheme (CPU* = bright green, IO = blue, Lock = red, etc.)
   - Color positions are stable (no jumping between frames)
   - AAS value in title updates each second

4. Press `down` — select the top wait type (e.g. CPU*), press `Enter` to drill in:
   - View switches to wait events within that type
   - Status bar still shows Live/interval/window
   - Rows show individual wait events (e.g. CPU*/ for CPU*)
   - Footer shows `Esc/b:back` hint

5. Press `down` to select a wait event, press `Enter` to drill into queries:
   - View shows query-level breakdown
   - Rows show truncated query text or query_id
   - Each row has TIME, %DB TIME, AAS, BAR columns

6. Press `Esc` — go back to wait events. Press `Esc` again — back to top level.

7. Press `l` — verify legend overlay:
   - 12 wait types listed with colored blocks, top-right of bar area
   - Press `l` again — legend disappears

8. Press `right` — verify zoom changes:
   - Bucket label updates (1s → 15s)
   - Refresh interval also changes (interval: 15s in status bar)
   - Window grows as data accumulates at new rate

9. Press `q` — verify clean exit back to rpg prompt

10. Kill load:
    ```bash
    kill $BENCH_PID
    ```

## Pass criteria

- Bars visible and colored correctly
- Y-axis labels readable
- Drill-down: Enter navigates in, Esc/b navigates back
- Selection highlight: bold+underline only (no color inversion)
- Legend overlay toggles with `l`
- Zoom changes refresh rate to match bucket size
- Window label shows actual data span, not ring-buffer capacity
- Clean exit on `q`
- GIF plays correctly in Telegram (resize to max 800px wide if needed)

## Recording demo GIF

```bash
# Start load
pgbench -i -s 1 postgresql://postgres@127.0.0.1:15433/ashtest 2>/dev/null
pgbench -c 8 -T 120 postgresql://postgres@127.0.0.1:15433/ashtest &
BENCH_PID=$!
sleep 5

# Record
expect tests/ai/ash-general.exp

# Convert
agg --theme dracula --font-size 13 demos/slash-ash-general.cast demos/slash-ash-general.gif
convert demos/slash-ash-general.gif -coalesce -resize 800x -layers optimize demos/slash-ash-general.gif

kill $BENCH_PID 2>/dev/null
```

Expect script (`tests/ai/ash-general.exp`):

```
#!/usr/bin/expect -f
set timeout 90
spawn env TERM=xterm-256color COLORTERM=truecolor COLUMNS=120 LINES=35 \
    asciinema rec demos/slash-ash-general.cast --overwrite --cols 120 --rows 35
expect -re {[$]\s*$} { sleep 0.5 }
send "./target/debug/rpg postgresql://postgres@127.0.0.1:15433/ashtest\r"
expect -re {[#>]\s*$} { sleep 1 }
send "/ash\r"
sleep 12
send "\033\[B"
sleep 1
send "\r"
sleep 6
send "\033\[B"
sleep 1
send "\r"
sleep 5
send "\033"
sleep 1
send "\033"
sleep 2
send "l"
sleep 3
send "l"
sleep 1
send "q"
sleep 2
send "\\q\r"
sleep 1
send "\x04"
sleep 2
```
