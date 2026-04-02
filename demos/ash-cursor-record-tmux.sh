#!/usr/bin/env bash
set -Eeuo pipefail

# Record /ash cursor demo using tmux + asciinema.
# Creates a tmux session, drives it with send-keys, captures with asciinema.

CAST_FILE="demos/ash-cursor-demo.cast"
GIF_FILE="demos/ash-cursor-demo.gif"
SESSION="ash-demo"
DB="demo_saas"
RPG="/Users/nik/github/rpg/target/debug/rpg"
WORKLOAD="/Users/nik/github/rpg/demos/ash-cursor-workload.sh"

# Clean up any previous session
tmux kill-session -t "${SESSION}" 2>/dev/null || true

# Start background workload
bash "${WORKLOAD}" &
WORKLOAD_PID=$!
trap 'kill ${WORKLOAD_PID} 2>/dev/null; wait ${WORKLOAD_PID} 2>/dev/null; tmux kill-session -t ${SESSION} 2>/dev/null' EXIT

sleep 3  # let workload warm up

# Start tmux with asciinema recording rpg
tmux new-session -d -s "${SESSION}" -x 160 -y 45 \
  "asciinema rec --cols 160 --rows 45 --overwrite '${CAST_FILE}' --command '${RPG} -d ${DB}'"

sleep 3  # wait for rpg to connect

send() { tmux send-keys -t "${SESSION}" "$@"; }

# Launch /ash
send '/ash' Enter
sleep 10  # let timeline fill up

# Zoom out to 15s buckets
send ']'
sleep 4

# Zoom back to 1s
send '['
sleep 4

# Pan left into history — cursor appears
for i in $(seq 1 12); do
  send Left
  sleep 0.8
done
sleep 2

# Pan right back
for i in $(seq 1 6); do
  send Right
  sleep 0.6
done
sleep 2

# Drill down into first wait type
send Enter
sleep 3

# Pan left in drilled view
for i in $(seq 1 5); do
  send Left
  sleep 0.8
done
sleep 2

# Esc back to live
send Escape
sleep 3

# Back to top level
send 'b'
sleep 2

# Toggle legend
send 'l'
sleep 3
send 'l'
sleep 1

# Quit /ash
send Escape
sleep 1

# Quit rpg
send '\q' Enter
sleep 2

# Wait for asciinema to finish
sleep 2

echo "Recording saved to ${CAST_FILE}"
echo "Converting to GIF..."
agg --cols 160 --rows 45 --speed 1 "${CAST_FILE}" "${GIF_FILE}"
echo "GIF saved to ${GIF_FILE}"
