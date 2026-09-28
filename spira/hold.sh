#!/usr/bin/env bash
#
# hold.sh — claim a bead for a non-aeon actor and keep it held.
#
#   hold.sh <bead-id>              claim and hold with this shell's pid
#   hold.sh <bead-id> --pid <P>    hold with pid P instead of $$
#   hold.sh <bead-id> --release    release a held bead (equivalent to unhold.sh)
#
# WHY THIS EXISTS. The reaper checks holder_alive, which looks for a pidfile named
# aeon-<fayth>-<id>.pid with a process whose argv contains aeon.sh. A non-aeon session
# (brain, concierge, a hand-run tool) passes neither test: no pidfile exists and no argv
# matches, so strand.sh reclaims the bead within seconds of the next pass. On 2026-09-07
# this cost sp-7pi eight attempts, none of which was a fact about the work.
#
# hold.sh writes hold-<id>.pid, which holder_alive checks by pid only (no argv test),
# and starts a background heartbeat that keeps the lease alive. unhold.sh (or --release)
# tears both down. A dead holder's pid vanishes from /proc and the bead is freed on the
# next sweep — the same liveness contract as an aeon, without requiring aeon.sh in argv.
#
# DO NOT USE THIS TO BYPASS THE LOOP. This is for work the operator ordered done by hand
# — a landing that the loop cannot perform, a fix applied at the keyboard. If the loop
# can do it, let the loop do it.
#
# THE CLAIM IS AN OPERATOR HOLD ON SPIRA-LC (sp-rlyl0), not a bd claim: HoldKind::Operator
# suspends dispatch without pretending this is a genuine Working-state claim, which nothing
# here actually is — the pidfile and heartbeat below are what stop the reaper regardless of
# whether the row is classified yet, so the lifecycle write is best-effort by the same
# construction every other lc.sh caller uses.
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/lib.sh"
. "$HERE/lc.sh"

ID=""; HOLD_PID=$$; MODE=hold
while [ $# -gt 0 ]; do
    case "$1" in
        --pid)     HOLD_PID="${2:?--pid needs a pid}"; shift ;;
        --release) MODE=release ;;
        -h|--help) sed -n '2,10p' "$0"; exit 0 ;;
        -*)        echo "hold.sh: unknown flag $1" >&2; exit 2 ;;
        *)         ID="$1" ;;
    esac
    shift
done
[ -n "$ID" ] || { echo "usage: hold.sh <bead-id> [--pid <P>] [--release]" >&2; exit 2; }

PIDFILE="$SPIRA_RUN/hold-$ID.pid"
HBFILE="${PIDFILE%.pid}.hb"

if [ "$MODE" = release ]; then
    exec "$HERE/unhold.sh" "$ID"
fi

# Refuse if an aeon already holds it — the hold is for NON-aeon work.
for apf in "$SPIRA_RUN"/aeon-*-"$ID".pid; do
    [ -e "$apf" ] || continue
    if aeon_alive "$apf"; then
        echo "hold.sh: $ID is held by a live aeon — slay it first or let it finish" >&2
        exit 1
    fi
done

# Refuse if already held by another hold.
if [ -f "$PIDFILE" ]; then
    existing="$(cat "$PIDFILE" 2>/dev/null)"
    if [ -n "$existing" ] && [ -d "/proc/$existing" ]; then
        echo "hold.sh: $ID is already held (pid $existing)" >&2
        exit 1
    fi
    rm -f "$PIDFILE" "$HBFILE"
fi

# Hold the bead on spira-lc — best-effort, like every other lc.sh caller (CANNOT_TELL until
# the bead is classified; a refusal here is logged and does not block the hold, since the
# pidfile below is what actually keeps the reaper off it).
lc_hold "$ID" operator "manual hold via hold.sh (pid $HOLD_PID)" "hold-$ID" >/dev/null 2>&1 || true

# Write the pidfile.
echo "$HOLD_PID" > "$PIDFILE"

# Start a background heartbeat so the lease does not expire under sentinel CHECK 2.
# Simpler than an aeon's: no stall detection, because the holder is a human at the
# keyboard. Beats every 120s (matching aeon.sh's default). Exits when the pidfile is
# removed (unhold.sh) or the holder pid dies.
#
# Redirected to /dev/null: the heartbeat inherits this script's file descriptors, and a
# $() substitution around the caller waits until EVERY writer on the pipe closes — so
# without this redirect, `out="$(hold.sh ...)"` blocks until the heartbeat exits.
#
# SIGTERM from unhold.sh kills this subshell. The trap kills the sleep child first so
# it does not become an orphan in the caller's process group and trigger the harness
# background-job check.
(
    _hb_sleep=
    _hb_term() { [ -n "$_hb_sleep" ] && kill "$_hb_sleep" 2>/dev/null; exit 0; }
    trap '_hb_term' TERM INT
    while true; do
        sleep "${SPIRA_HOLD_HEARTBEAT:-120}" &
        _hb_sleep=$!
        wait "$_hb_sleep" 2>/dev/null
        _hb_sleep=
        [ -f "$PIDFILE" ] || exit 0
        [ -d "/proc/$HOLD_PID" ] || exit 0
        bdq heartbeat "$ID" >/dev/null 2>&1 || exit 0
    done
) </dev/null >/dev/null 2>&1 &
echo $! > "$HBFILE"

printf 'held %s (pid %s, heartbeat %s)\n' "$ID" "$HOLD_PID" "$(cat "$HBFILE")"
