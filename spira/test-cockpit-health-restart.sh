#!/usr/bin/env bash
#
# test-cockpit-health-restart.sh — health.sh survives a spira.conf change in place; ensure
# recreates it if the pane dies outright.
#
# THE FAILURE THIS SUITE EXISTS FOR (sp-94yqa). health.sh runs directly in a tmux pane, with
# no wrapper and no supervisor. On a config change it printed "exiting for restart" and
# exited 0 — but nothing restarts a tmux pane's command; the pane just closed, taking its
# @cockpit tag with it. `layout.sh ensure` found the window only through a pane still
# carrying that tag, so once the tag died with the pane the dashboard was never repaired.
# Every spira.conf edit silently removed the operator's ops pane.
#
# TWO CASES:
#   A. the common path — health.sh re-execs itself on a config change instead of exiting,
#      so the pane and its tag both survive.
#   B. defence in depth — if the pane dies anyway (killed, crashed), `ensure` must still
#      find and repair the window even though no pane in it carries an @cockpit tag any
#      more. `layout.sh up` now marks the WINDOW itself (`@cockpit_up`), which survives
#      every dashboard pane dying.
#
# covers: cockpit/health.sh cockpit/layout.sh
set -uo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
COCKPIT_DIR="$(dirname "$HERE")/cockpit"
LAYOUT="$COCKPIT_DIR/layout.sh"
HEALTH="$COCKPIT_DIR/health.sh"

pass=0; fail=0
ok()   { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()  { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()   { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }

command -v tmux >/dev/null 2>&1 || { echo "SKIP: tmux not available" >&2; exit 0; }

TMP="$(mktemp -d)"
TMUXDIR="$TMP/tmux-fixture"; mkdir -p "$TMUXDIR"
export TMUX_TMPDIR="$TMUXDIR"
unset TMUX TMUX_PANE
cleanup() { tmux kill-server 2>/dev/null || true; rm -rf "$TMP"; }
trap cleanup EXIT

mkdir -p "$TMP/.runtime"

# A mock systemctl that always reports active, so health.sh's halt_banner does not render —
# same seam test-conf-watch.sh uses.
MOCK_SYSTEMCTL="$TMP/mock-systemctl"
printf '#!/bin/sh\necho active\n' > "$MOCK_SYSTEMCTL"
chmod +x "$MOCK_SYSTEMCTL"

TICK=0.2
CONF="$TMP/spira.conf"
printf '# test\n' > "$CONF"

tmux new-session -d -s cockpit -x 200 -y 50
WIN=cockpit:0
SESS=$(tmux list-panes -t "$WIN" -F '#{pane_id}')

# ── Case A: the pane persists across a config change, execing in place ────────────────
# Every var the real health.sh loop needs is embedded directly in the pane's own command
# line — the fixture must not depend on whatever environment happened to start the tmux
# server.
HEALTH_CMD="SPIRA_CONF='$CONF' SPIRA_HOME='$HERE' SPIRA_REPO='$TMP' SPIRA_RUN='$TMP/.runtime' \
SPIRA_DB='$TMP/nodb' SPIRA_REPO_MAP='$TMP/no-map' SPIRA_GOAL=sp-test SPIRA_FAYTHS=t \
SPIRA_SYSTEMCTL='$MOCK_SYSTEMCTL' SPIRA_HEALTH_TICK=$TICK bash '$HEALTH' loop"
HP=$(tmux split-window -P -F '#{pane_id}' -d -h -t "$SESS" "$HEALTH_CMD")
tmux set-option -p -t "$HP" @cockpit health
sleep 0.6

[ "$(tmux list-panes -t "$WIN" -F '#{pane_id}' | grep -Fxc "$HP")" = "1" ] \
    || { echo "fixture: health pane did not start" >&2; exit 1; }

# Pre-date so the touch below produces a genuine mtime change without waiting out a real
# second of wall clock for the two stat reads to land apart.
touch -d "3 seconds ago" "$CONF"
touch "$CONF"
sleep 1.2

alive=$(tmux list-panes -t "$WIN" -F '#{pane_id}' | grep -Fxc "$HP")
is "A: health pane survives a config change" "1" "$alive"
is "A: health pane keeps its @cockpit tag" "health" "$(tmux display -p -t "$HP" '#{@cockpit}' 2>/dev/null)"

out="$(tmux capture-pane -p -t "$HP" 2>/dev/null)"
[[ "$out" == *"config changed"* ]] \
    && ok "A: pane logged the restart" \
    || bad "A: pane logged the restart" "pane output: $out"

# Prove it is genuinely still looping (a new process), not a dead shell about to be
# reaped: give it two more ticks and confirm it is still alive and still tagged.
sleep $(awk "BEGIN{print $TICK*2}")
is "A: health pane is still alive after the restart" "1" \
    "$(tmux list-panes -t "$WIN" -F '#{pane_id} #{pane_dead}' | awk -v p="$HP" '$1==p{print ($2=="0")?1:0}')"
is "A: health pane still tagged after the restart" "health" "$(tmux display -p -t "$HP" '#{@cockpit}' 2>/dev/null)"

tmux kill-pane -t "$HP" 2>/dev/null || true

# ── Case B: the pane dies outright — ensure must still find and heal the window ───────
# A stub stands in for health.sh here: this case is about `ensure` recognising an up
# window with no tagged pane left, not about the real loop's own restart behaviour
# (Case A already covers that against the genuine script).
FAKE_COCK="$TMP/fake-cockpit"; mkdir -p "$FAKE_COCK"
printf '#!/usr/bin/env bash\nsleep 300\n' > "$FAKE_COCK/health.sh"
chmod +x "$FAKE_COCK/health.sh"

tmux new-session -d -s brain -x 200 -y 50
WIN2=brain:0
SESS2=$(tmux list-panes -t "$WIN2" -F '#{pane_id}')
HP2=$(tmux split-window -P -F '#{pane_id}' -d -h -t "$SESS2" "bash '$FAKE_COCK/health.sh' loop")
tmux set-option -p -t "$HP2" @cockpit health
# What `up` sets on a real cockpit — marks the WINDOW, independent of any pane tag.
tmux set-option -w -t "$WIN2" @cockpit_up 1
sleep 0.3

tmux kill-pane -t "$HP2" 2>/dev/null || true
sleep 0.2

is "SEEN RED: no pane in the window is tagged" "" \
    "$(tmux list-panes -t "$WIN2" -F '#{@cockpit}' 2>/dev/null | grep -v '^$')"

SPIRA_COCKPIT="$FAKE_COCK" \
SPIRA_REPO="$TMP" \
SPIRA_RUN="$TMP/.runtime" \
SPIRA_HOME="$HERE" \
SPIRA_CONF="$TMP/no.conf" \
COCKPIT_CLIENT_IDLE_SECS=0 \
    bash "$LAYOUT" ensure >/dev/null 2>&1 || true
sleep 0.5

is "B: ensure recreated the health pane" "1" \
    "$(tmux list-panes -t "$WIN2" -F '#{@cockpit}' 2>/dev/null | grep -Fxc health)"

printf '\ntest-cockpit-health-restart: %d ok, %d fail\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
