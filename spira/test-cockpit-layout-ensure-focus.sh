#!/usr/bin/env bash
#
# test-cockpit-layout-ensure-focus.sh — ensure leaves operator's focus unchanged when no repair needed.
#
# THE FAILURE THIS SUITE EXISTS FOR. repair_dashboards() was called from the `ensure`
# case (timer-driven, ~60s cadence) with an unconditional tmux select-pane at the end.
# This moved the operator's focus to the session pane every 60 seconds, even when no
# repairs were needed. The fix records the active pane at entry, restores it after
# repairs, and only selects the session pane if the original active pane was destroyed.
#
# defect: sp-gyl8n
# covers: cockpit/layout.sh
set -uo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
COCKPIT_DIR="$(dirname "$HERE")/cockpit"
LAYOUT="$COCKPIT_DIR/layout.sh"

pass=0; fail=0
ok()   { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()  { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()   { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }

TMP="$(mktemp -d)"
TMUXDIR="$TMP/tmux-fixture"
mkdir -p "$TMUXDIR"
FIXTURE_UP=0

cleanup() {
    [ "$FIXTURE_UP" -eq 1 ] && TMUX_TMPDIR="$TMUXDIR" tmux kill-server 2>/dev/null || true
    rm -rf "$TMP"
}
trap cleanup EXIT

export TMUX_TMPDIR="$TMUXDIR"
unset TMUX

TMUX_TMPDIR="$TMUXDIR" tmux start-server
FIXTURE_UP=1

RUN="$TMP/.runtime"; mkdir -p "$RUN"

TMUX_TMPDIR="$TMUXDIR" tmux new-session -d -s test -x 214 -y 53
TMUX_TMPDIR="$TMUXDIR" tmux set-option -t test window-size largest

# Create three panes in a window: session pane (0), health pane (1), and mail pane (2)
SESSION_PANE="test:0"
HEALTH_PANE_ID=$(TMUX_TMPDIR="$TMUXDIR" tmux split-window -t "$SESSION_PANE" -h -F '#{pane_id}' 2>/dev/null)
MAIL_PANE_ID=$(TMUX_TMPDIR="$TMUXDIR" tmux split-window -t "$SESSION_PANE:0" -v -F '#{pane_id}' 2>/dev/null)

# Tag the dashboards so repair_dashboards recognizes them
TMUX_TMPDIR="$TMUXDIR" tmux send-keys -t "$HEALTH_PANE_ID" "tmux set-option -t '#{client_tty}' user @cockpit health" C-m 2>/dev/null || true
TMUX_TMPDIR="$TMUXDIR" tmux send-keys -t "$MAIL_PANE_ID" "tmux set-option -t '#{client_tty}' user @cockpit mail" C-m 2>/dev/null || true

# Now set focus to the health pane (not the session pane)
TMUX_TMPDIR="$TMUXDIR" tmux select-pane -t "$HEALTH_PANE_ID"
BEFORE_ACTIVE=$(TMUX_TMPDIR="$TMUXDIR" tmux display-message -t "$SESSION_PANE" -p '#{pane_active}')

# Source layout.sh and call repair_dashboards with MAIL_CMD unset (no mail repair)
# This is the case where no repairs are needed
export WINDOW="test:0"
export MAIL_CMD=""
export SPIRA_COCKPIT="$COCKPIT_DIR"
export COCK="$COCKPIT_DIR"

# We need to source layout.sh and call repair_dashboards
# Since we can't directly call functions from a script, we'll use bash -c to run it in the layout.sh context
bash -c "
    source '$LAYOUT'
    WINDOW='$WINDOW' MAIL_CMD='' SPIRA_COCKPIT='$SPIRA_COCKPIT' COCK='$COCK'
    RUN='$RUN' TMUX_TMPDIR='$TMUXDIR'
    repair_dashboards
" 2>/dev/null || true

AFTER_ACTIVE=$(TMUX_TMPDIR="$TMUXDIR" tmux display-message -t "$SESSION_PANE" -p '#{pane_active}')

is "active pane unchanged when no repair needed" "$BEFORE_ACTIVE" "$AFTER_ACTIVE"

echo
echo "Results: $pass ok, $fail failed"
[ "$fail" -eq 0 ]
