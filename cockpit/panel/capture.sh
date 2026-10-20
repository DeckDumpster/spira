#!/usr/bin/env bash
# Screenshot the attention pane, with no human and no live database.
#
#   ./capture.sh <binary> <out-prefix> [sel] [WxH] [keys] [text]
#
# Runs the panel against tests/fixture.json in a scratch tmux pane of the REAL cockpit size,
# lets it paint, and writes <prefix>.txt (structure) and <prefix>.svg (colour).
#
# It is a real tmux pane on purpose. `--once` prints lines to stdout and never exercises the
# paint loop — the cursor-home write, the per-line erase, the deliberate absence of a
# trailing newline — so a frame that is correct on stdout can still scroll its top row away
# on screen. That failure has happened here before.
set -euo pipefail
cd "$(dirname "$0")"
BIN="${1:?usage: capture.sh <binary> <out-prefix> [sel] [WxH]}"
OUT="${2:?}"
SEL="${3:-0}"
SIZE="${4:-107x19}"
KEYS="${5:-}"   # a key SEQUENCE, e.g. "Tab o" — switch to INSIGHTS, then open the reader
# TEXT is typed LITERALLY after the keys, which is the only way to shoot the answer field:
# send-keys reads its arguments as key NAMES, so a space in an answer would arrive as the
# Space key name at best and be swallowed at worst, and 200 characters at one key per 0.4s
# is a minute and a half per shot.
TEXT="${6:-}"
W="${SIZE%x*}"; H="${SIZE#*x}"

# FREEZE THE CLOCK, not only the data. Every stamp in the pane is now an age, so a capture
# taken against the wall clock differs from one taken a minute earlier — which is exactly
# the coin toss tests/fixture.json exists to remove, one layer up. PANEL_NOW is the other
# half of that fixture and is not optional here.
#
# The instant is deliberately just after the newest row in the fixture, so a shot shows the
# real spread the pane has to render: minutes at the bottom of the queue, days at the top.
NOW="${PANEL_NOW:-2026-09-05T16:30:00Z}"
S="panecap-$$"

tmux kill-session -t "$S" 2>/dev/null || true
tmux new-session -d -s "$S" -c "$PWD" \
  "PANEL_FIXTURE=$PWD/tests/fixture.json PANEL_NOW=$NOW '$BIN' --sel $SEL"
tmux set-option -t "$S" window-size manual
tmux resize-window -t "$S" -x "$W" -y "$H"
# Let it paint. The store loads the fixture synchronously, so this is paint time only.
for _ in $(seq 40); do
    [ "$(tmux capture-pane -p -t "$S" | grep -c 'view')" -gt 0 ] && break
    sleep 0.1
done
# KEYS is a SEQUENCE, sent one key at a time with a pause between. "Tab o" has to reach the
# reader of the second tab, and both keys arriving in one write can be read as one event.
for k in $KEYS; do
    tmux send-keys -t "$S" "$k"
    sleep 0.4
done
if [ -n "$TEXT" ]; then
    tmux send-keys -t "$S" -l "$TEXT"
    sleep 0.6
fi
tmux capture-pane -p    -t "$S" > "$OUT.txt"
tmux capture-pane -p -e -t "$S" | "$(dirname "$BIN")/ansi-svg" > "$OUT.svg"
tmux kill-session -t "$S" 2>/dev/null || true
printf '%s\n' "$OUT.txt $(wc -l < "$OUT.txt") rows" "$OUT.svg"
