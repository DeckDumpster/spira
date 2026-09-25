#!/usr/bin/env bash
#
# test-cockpit-layout.sh — ensure detaches idle ghost clients and window height stabilises.
#
# THE FAILURE THIS SUITE EXISTS FOR. Ghost clients (terminals idle for days but still
# attached to the tmux server) drove the cockpit's NEXT/RECENT sections to flap between
# ~5 and ~14 rows on every tick. With window-size latest, a ghost that became "latest"
# shrank the shared window to its small terminal height until the live client regained
# "latest". layout.sh ensure now detaches clients idle longer than
# COCKPIT_CLIENT_IDLE_SECS (default 6 h); layout.sh up sets window-size largest so the
# biggest client's height governs regardless of which client was most recently active.
#
# defect: sp-eq5
# covers: cockpit/layout.sh
set -uo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
COCKPIT_DIR="$(dirname "$HERE")/cockpit"
LAYOUT="$COCKPIT_DIR/layout.sh"

pass=0; fail=0
ok()   { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()  { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()   { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want() { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }

if ! command -v script >/dev/null 2>&1; then
    echo "SKIP: 'script' not available" >&2
    exit 0
fi

TMP="$(mktemp -d)"
TMUXDIR="$TMP/tmux-fixture"
mkdir -p "$TMUXDIR"
FIXTURE_UP=0
GHOST_PID=0; LIVE_PID=0
# FIFOs keep the write end open so script does not forward EOF to the PTY, which
# would cause tmux to detach the client immediately.
GHOST_FIFO="$TMP/ghost.fifo"
LIVE_FIFO="$TMP/live.fifo"
mkfifo "$GHOST_FIFO" "$LIVE_FIFO"

cleanup() {
    # Kill the fixture server FIRST. tmux kill-server disconnects every attached
    # client (tmux attach-session), which causes script(1) to see its child exit
    # and exit in turn.
    [ "$FIXTURE_UP" -eq 1 ] && TMUX_TMPDIR="$TMUXDIR" tmux kill-server 2>/dev/null || true
    # Closing the <> fds drops the last write-end reference on each FIFO; belt-and-suspenders
    # in case script is still waiting on stdin after the server dies.
    { exec 3>&-; } 2>/dev/null || true
    { exec 4>&-; } 2>/dev/null || true
    # Allow time for the server→client→script exit cascade to propagate.
    sleep 0.2
    # Kill the entire process GROUP of each client stub, not just the script process.
    # script spawns sh which spawns tmux attach-session; killing script alone orphans
    # those grandchildren in the suite's process group (PGID is inherited and survives
    # reparenting), where suites.sh finds them with kill -0 -- -pgid and marks the suite
    # red. Each stub was spawned with setsid so its PGID = its PID; kill -- -$PID reaches
    # all descendants.
    [ "${GHOST_PID:-0}" -gt 0 ] && kill -- -"$GHOST_PID" 2>/dev/null || true
    [ "${LIVE_PID:-0}" -gt 0 ]  && kill -- -"$LIVE_PID"  2>/dev/null || true
    # Reap the direct bash-tracked background jobs (the script processes themselves).
    wait 2>/dev/null || true
    rm -rf "$TMP"
}
trap cleanup EXIT

# ── Fixture server ──────────────────────────────────────────────────────────────
# TMUX_TMPDIR redirects all bare 'tmux' calls (inside layout.sh) to $TMUXDIR/default
# so ensure operates on the fixture rather than the operator's live server.
export TMUX_TMPDIR="$TMUXDIR"
unset TMUX

TMUX_TMPDIR="$TMUXDIR" tmux start-server
FIXTURE_UP=1

RUN="$TMP/run"; mkdir -p "$RUN"
# This fixture never runs `layout.sh up`, so there is no `@cockpit` pane anywhere and
# `ensure` would otherwise read that as the cockpit having crashed and try to rebuild it
# from scratch (see test-cockpit-down-marker.sh) — noise this suite has no fixture for.
# The down marker is what a real deliberate absence looks like, and is not what this test
# is about, so mark it and let `ensure` skip past that branch.
: > "$RUN/cockpit.down"

# Create a cockpit session. window-size largest is what 'up' sets; we set it here
# so the detachment test can assert against it directly without invoking 'up'
# (which would launch health.sh and panel-run.sh as background panes).
TMUX_TMPDIR="$TMUXDIR" tmux new-session -d -s cockpit -x 214 -y 53
TMUX_TMPDIR="$TMUXDIR" tmux set-option -t cockpit window-size largest

# ── POSITIVE CONTROL: attach a ghost client first ───────────────────────────────
# script(1) provides a real PTY so tmux registers this as a connected client.
# TERM=xterm-256color is required — without it tmux's attach-session refuses with
# "terminal does not support clear". The FIFO keeps the write end open so script
# does not receive EOF on stdin and does not forward it to the PTY (which would
# cause tmux to detach).
# Open FIFO read/write (<>) so the open does not block waiting for the other end.
# The write-end reference keeps script's FIFO reads blocking (no EOF) as long as
# fd 3 is open. When cleanup closes fd 3 the FIFO write end drops to zero
# references and script gets EOF, which propagates to tmux and detaches the client.
exec 3<>"$GHOST_FIFO"
# setsid gives script its own process group (PGID = script's PID) so cleanup can
# kill the whole group — script + sh + tmux attach-session — without touching the
# suite's own group.
TMUX= TMUX_TMPDIR="$TMUXDIR" \
    setsid script -q -c "TERM=xterm-256color tmux attach-session -t cockpit" /dev/null \
    < "$GHOST_FIFO" >/dev/null 2>&1 &
GHOST_PID=$!
sleep 0.5

GHOST_TTY=$(TMUX_TMPDIR="$TMUXDIR" tmux list-clients -F '#{client_tty}' 2>/dev/null | head -1)
if [ -z "$GHOST_TTY" ]; then
    echo "SKIP: stub client did not connect (PTY attachment requires a real terminal)" >&2
    exit 0
fi
ok "ghost client attached ($GHOST_TTY)"

# Sleep past the idle threshold we will pass to ensure (2 s).
sleep 2.1

# Attach the live client — most recently active.
exec 4<>"$LIVE_FIFO"
TMUX= TMUX_TMPDIR="$TMUXDIR" \
    setsid script -q -c "TERM=xterm-256color tmux attach-session -t cockpit" /dev/null \
    < "$LIVE_FIFO" >/dev/null 2>&1 &
LIVE_PID=$!
sleep 0.5

clients_before=$(TMUX_TMPDIR="$TMUXDIR" tmux list-clients 2>/dev/null | wc -l)
is "two clients attached before ensure" "2" "$clients_before"

# Capture window height while both clients are attached as the expected baseline.
# #{window_height} is the usable area (client_height minus status lines), which is
# the value tmux reports for the window, not for the client's terminal.
WH_EXPECTED=$(TMUX_TMPDIR="$TMUXDIR" tmux display-message -t cockpit -p '#{window_height}' 2>/dev/null)
[ -n "$WH_EXPECTED" ] || WH_EXPECTED=23

# ── Run ensure ──────────────────────────────────────────────────────────────────
# SPIRA_COCKPIT matches BASH_SOURCE[0]'s directory so the copy-guard in ensure
# passes (it compares realpath of the running script to realpath of
# $SPIRA_COCKPIT/layout.sh; they are identical when we point it at the worktree).
# SPIRA_CONF names a non-existent file so conf.sh skips operator config.
# COCKPIT_CLIENT_IDLE_SECS=2: detach clients idle > 2 s — the ghost (~2.6 s old)
# is detached; the live (< 1 s old) survives.
SPIRA_COCKPIT="$COCKPIT_DIR" \
SPIRA_REPO="$TMP" \
SPIRA_RUN="$RUN" \
SPIRA_HOME="$HERE" \
SPIRA_CONF="$TMP/no.conf" \
COCKPIT_CLIENT_IDLE_SECS=2 \
    bash "$LAYOUT" ensure 2>/dev/null || true

sleep 0.2

# ── Assertions ──────────────────────────────────────────────────────────────────
clients_after=$(TMUX_TMPDIR="$TMUXDIR" tmux list-clients 2>/dev/null | wc -l)
is "one client remains after ensure" "1" "$clients_after"

remaining_tty=$(TMUX_TMPDIR="$TMUXDIR" tmux list-clients -F '#{client_tty}' 2>/dev/null | head -1)
[ "$remaining_tty" != "$GHOST_TTY" ] \
    && ok "ghost ($GHOST_TTY) detached; live client remains" \
    || bad "ghost detached" "ghost TTY $GHOST_TTY is still listed"

# Window height must equal the baseline captured while the live client was attached.
# With window-size largest and only the live client remaining after the ghost is
# detached, the window stays at the live client's height rather than shrinking.
wh_after=$(TMUX_TMPDIR="$TMUXDIR" tmux display-message -t cockpit -p '#{window_height}' 2>/dev/null)
is "window height equals live-client height ($WH_EXPECTED) after ensure" "$WH_EXPECTED" "$wh_after"

# Heal log records each detach so the operator can audit which ghosts were removed.
HEAL="$TMP/run/cockpit-heal.log"
if [ -f "$HEAL" ]; then
    ok "heal.log written"
    want "heal.log records ghost TTY" "$GHOST_TTY" "$(cat "$HEAL")"
else
    bad "heal.log written" "not found at $HEAL"
fi

printf '\ntest-cockpit-layout: %d ok, %d fail\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
