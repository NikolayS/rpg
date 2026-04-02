#!/usr/bin/env bash
set -Eeuo pipefail

# Script to record /ash cursor demo via asciinema.
# Drives rpg interactively by sending keystrokes via a FIFO.
#
# Usage: asciinema rec --command "bash demos/ash-cursor-record.sh" demos/ash-cursor-demo.cast

export PATH="/Users/nik/github/rpg/target/debug:${PATH}"
DB="demo_saas"

# Start background workload
bash /Users/nik/github/rpg/demos/ash-cursor-workload.sh &
WORKLOAD_PID=$!
trap 'kill ${WORKLOAD_PID} 2>/dev/null; wait ${WORKLOAD_PID} 2>/dev/null' EXIT

# Wait for workload to generate some activity
sleep 2

# Create a FIFO to feed keystrokes to rpg
FIFO=$(mktemp -u /tmp/rpg-demo-XXXXXX)
mkfifo "${FIFO}"
trap 'kill ${WORKLOAD_PID} 2>/dev/null; wait ${WORKLOAD_PID} 2>/dev/null; rm -f ${FIFO}' EXIT

# Start rpg reading from the FIFO for programmatic input
# but with terminal for TUI rendering
(
  sleep 3
  # Type /ash and Enter
  printf '/ash\n' > "${FIFO}"
  sleep 8

  # Zoom out ]
  printf ']' > "${FIFO}"
  sleep 3

  # Zoom back [
  printf '[' > "${FIFO}"
  sleep 3

  # Pan left (Esc[D = Left arrow) — 10 times
  for i in $(seq 1 10); do
    printf '\033[D' > "${FIFO}"
    sleep 1
  done
  sleep 2

  # Pan right — 5 times
  for i in $(seq 1 5); do
    printf '\033[C' > "${FIFO}"
    sleep 1
  done
  sleep 2

  # Drill down (Enter)
  printf '\n' > "${FIFO}"
  sleep 3

  # Pan left in drilled view — 5 times
  for i in $(seq 1 5); do
    printf '\033[D' > "${FIFO}"
    sleep 1
  done
  sleep 2

  # Esc back to live
  printf '\033' > "${FIFO}"
  sleep 3

  # Back (b)
  printf 'b' > "${FIFO}"
  sleep 2

  # Quit
  printf 'q' > "${FIFO}"
  sleep 1

  # Quit rpg
  printf '\\q\n' > "${FIFO}"
  sleep 1
) &

rpg -d "${DB}" < "${FIFO}"
