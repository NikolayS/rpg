# AI test: slash-ash-general

Tests the `/ash` Active Session History TUI — basic functionality, visual correctness,
and demo recording.

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
   - Status bar shows: `/ash  [Live]  interval: 1s  window: 10min  active: N`
   - Timeline block has visible border, title shows zoom level and AAS value
   - Y-axis labels are readable (not invisible against background)
   - Bars appear on the right side and grow left over time

3. Wait 10+ seconds — verify bars accumulate correctly:
   - Colors match pg_ash scheme (CPU* = bright green, IO = blue, Lock = red, etc.)
   - Color positions are stable (no jumping between frames)
   - AAS value in title updates each second

4. Press `down` twice — verify row selection:
   - Selected row is bold + underlined
   - No color inversion (colors remain correct on selected row)

5. Press `l` — verify legend overlay:
   - 12 wait types listed with colored blocks, top-right of bar area
   - Press `l` again — legend disappears

6. Press `left` / `right` — verify zoom changes:
   - Title zoom label updates (1s -> 5s -> 30s -> 5min)
   - Bar aggregation changes accordingly

7. Press `q` — verify clean exit back to rpg prompt

8. Kill load:
   ```bash
   kill $BENCH_PID
   ```

## Recording demo GIF

```bash
# Record using expect script
expect tests/ai/ash-general.exp

# Convert
agg --theme dracula --font-size 13 demos/slash-ash-general.cast demos/slash-ash-general.gif

# Resize if over 800px wide (Telegram limit for inline playback)
convert demos/slash-ash-general.gif -coalesce -resize 800x -layers optimize demos/slash-ash-general.gif

# Commit both cast and gif
git add demos/slash-ash-general.gif demos/slash-ash-general.cast
git commit -m "docs(ash): update demo GIF"
```

Expect script (`tests/ai/ash-general.exp`):

```
#!/usr/bin/expect -f
set timeout 60
spawn env TERM=xterm-256color COLORTERM=truecolor COLUMNS=120 LINES=35 \
    asciinema rec demos/slash-ash-general.cast --overwrite --cols 120 --rows 35
expect -re {[$]\s*$} { sleep 0.5 }
send "./target/debug/rpg postgresql://postgres@127.0.0.1:15433/ashtest\r"
expect -re {[#>]\s*$} { sleep 1 }
send "/ash\r"
sleep 1
sleep 20
send "q"
sleep 2
send "\\q\r"
sleep 1
send "\x04"
sleep 2
```

## Pass criteria

- Bars visible and colored correctly
- Y-axis labels readable
- Selection highlight: bold+underline only (no color inversion)
- Legend overlay toggles with `l`
- Zoom changes with arrow keys
- Clean exit on `q`
- GIF plays correctly in Telegram (resize to max 800px wide if needed)
