#!/usr/bin/env bash
#
# test-cockpit-layout-ensure-focus.sh — repair_dashboards restores the operator's actual
# focus, and only falls back to the session pane when that focus is gone.
#
# THE FAILURE THIS SUITE EXISTS FOR. repair_dashboards() was called from the `ensure`
# case (timer-driven, ~60s cadence) with an unconditional tmux select-pane at the end.
# This moved the operator's focus to the session pane every 60 seconds, even when no
# repairs were needed. The fix records the active pane at entry, restores it after
# repairs, and only selects the session pane if the original active pane was destroyed.
#
# THE ORIGINAL ASSERTION WAS VACUOUS. It compared #{pane_active} of the WINDOW'S
# active pane against itself — whichever pane tmux considers active always reports
# pane_active=1, so the case could not fail no matter what repair_dashboards did. This
# rewrite compares the active pane's #{pane_id} before and after, which is the only
# thing that can actually move, and adds the positive control: when the originally
# active pane really is gone, focus MUST move to the session pane.
#
# defect: sp-gyl8n
# covers: cockpit/layout.sh
set -uo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"
COCKPIT_DIR="$(dirname "$HERE")/cockpit"
LAYOUT="$COCKPIT_DIR/layout.sh"

command -v tmux >/dev/null 2>&1 || { echo "  SKIP  tmux is not on PATH"; exit 77; }

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
unset TMUX TMUX_PANE

TMUX_TMPDIR="$TMUXDIR" tmux start-server
FIXTURE_UP=1

RUN="$TMP/.runtime"; mkdir -p "$RUN"

# well_formed <session-name> -> sets up a 3-pane window (session, health, mail — all
# tagged so repair_dashboards finds nothing to fix). Prints "<session-pane-id>
# <health-pane-id> <mail-pane-id>".
well_formed() {
    local sname="$1"
    TMUX_TMPDIR="$TMUXDIR" tmux new-session -d -s "$sname" -x 214 -y 53
    TMUX_TMPDIR="$TMUXDIR" tmux set-option -t "$sname" window-size largest

    local sess="$sname:0"
    local health mail
    health=$(TMUX_TMPDIR="$TMUXDIR" tmux split-window -t "$sess" -h -P -F '#{pane_id}')
    mail=$(TMUX_TMPDIR="$TMUXDIR" tmux split-window -t "$sess:0" -v -P -F '#{pane_id}')
    local sess_id
    sess_id=$(TMUX_TMPDIR="$TMUXDIR" tmux list-panes -t "$sess" -F '#{pane_id}' | head -1)

    TMUX_TMPDIR="$TMUXDIR" tmux set-option -p -t "$health" @cockpit health
    TMUX_TMPDIR="$TMUXDIR" tmux set-option -p -t "$mail" @cockpit mail

    echo "$sess_id $health $mail"
}

# with_duplicate <session-name> -> a session pane plus TWO panes tagged health, as a
# respawn racing repair_dashboards can produce. repair_dashboards's own dedup step
# kills every health pane but the lowest-index one — this is the fixture that makes
# repair_dashboards itself destroy the pane the operator had focused, which is the only
# way the case this suite exists for actually happens. Prints "<session-pane-id>
# <kept-health-id> <duplicate-health-id-to-be-killed>".
with_duplicate() {
    local sname="$1"
    TMUX_TMPDIR="$TMUXDIR" tmux new-session -d -s "$sname" -x 214 -y 53
    TMUX_TMPDIR="$TMUXDIR" tmux set-option -t "$sname" window-size largest

    local sess="$sname:0"
    local h1 h2 sess_id
    h1=$(TMUX_TMPDIR="$TMUXDIR" tmux split-window -t "$sess" -h -P -F '#{pane_id}')
    h2=$(TMUX_TMPDIR="$TMUXDIR" tmux split-window -t "$sess" -h -P -F '#{pane_id}')
    sess_id=$(TMUX_TMPDIR="$TMUXDIR" tmux list-panes -t "$sess" -F '#{pane_id}' | head -1)

    TMUX_TMPDIR="$TMUXDIR" tmux set-option -p -t "$h1" @cockpit health
    TMUX_TMPDIR="$TMUXDIR" tmux set-option -p -t "$h2" @cockpit health

    echo "$sess_id $h1 $h2"
}

call_repair() {   # call_repair <window> -> nothing; runs repair_dashboards sourced
    local window="$1"
    TMUX_TMPDIR="$TMUXDIR" env -i HOME="$TMP" PATH="/usr/bin:/bin" TMUX_TMPDIR="$TMUXDIR" \
        WINDOW="$window" MAIL_CMD="" SPIRA_REPO="$TMP" SPIRA_COCKPIT="$COCKPIT_DIR" \
        SPIRA_RUN="$RUN" SPIRA_INSTANCE=fixture SPIRA_LOOM_BIN="" COCKPIT_CWD="$TMP" \
        COCKPIT_BOTTOM_PCT=30 COCKPIT_RIGHT_PCT=33 \
        bash -c '. "'"$LAYOUT"'"; repair_dashboards' 2>/dev/null || true
}

active_of() {   # active_of <window> -> pane_id of the active pane
    TMUX_TMPDIR="$TMUXDIR" tmux display-message -t "$1" -p '#{pane_id}' 2>/dev/null
}

echo "test-cockpit-layout-ensure-focus.sh"

echo
echo "no repair needed: focus on a non-session pane survives repair_dashboards"
read -r sess_id health mail < <(well_formed w1)
TMUX_TMPDIR="$TMUXDIR" tmux select-pane -t "$health"
before="$(active_of w1:0)"
is "fixture: health pane is active before the call" "$health" "$before"
call_repair "w1:0"
after="$(active_of w1:0)"
is "active pane (by id) is unchanged when nothing needed repair" "$before" "$after"

echo
echo "positive control: repair_dashboards' own dedup kills the pane the operator had"
echo "focused -- focus must fall back to the session pane, not stay on the corpse"
read -r sess_id2 kept dup < <(with_duplicate w2)
TMUX_TMPDIR="$TMUXDIR" tmux select-pane -t "$dup"
before2="$(active_of w2:0)"
is "fixture: the soon-to-be-killed duplicate is active before the call" "$dup" "$before2"
call_repair "w2:0"
still_there="$(TMUX_TMPDIR="$TMUXDIR" tmux list-panes -t w2:0 -F '#{pane_id}' | grep -Fxc "$dup")" || still_there=0
is "the duplicate really was killed by repair_dashboards" "0" "$still_there"
after2="$(active_of w2:0)"
is "focus falls back to the session pane once its target is gone" "$sess_id2" "$after2"

tl_summary
