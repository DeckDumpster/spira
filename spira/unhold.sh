#!/usr/bin/env bash
#
# unhold.sh — release a manual hold on a bead.
#
#   unhold.sh <bead-id>
#
# Kills the heartbeat, removes the pidfile, and releases the manual hold on spira-lc
# (sp-rlyl0) that hold.sh applied — best-effort, the same construction every spira-lc caller-verb call
# uses. The bead is available for the loop to pick up as soon as nothing else holds it.
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/lib.sh"

ID=""
while [ $# -gt 0 ]; do
    case "$1" in
        -h|--help) sed -n '2,6p' "$0"; exit 0 ;;
        -*)        echo "unhold.sh: unknown flag $1" >&2; exit 2 ;;
        *)         ID="$1" ;;
    esac
    shift
done
[ -n "$ID" ] || { echo "usage: unhold.sh <bead-id>" >&2; exit 2; }

PIDFILE="$SPIRA_RUN/hold-$ID.pid"
HBFILE="${PIDFILE%.pid}.hb"

if [ ! -f "$PIDFILE" ]; then
    echo "unhold.sh: no hold pidfile for $ID" >&2
    exit 1
fi

# Kill the heartbeat first, then remove files.
if [ -f "$HBFILE" ]; then
    hbpid="$(cat "$HBFILE" 2>/dev/null)"
    [ -n "$hbpid" ] && kill "$hbpid" 2>/dev/null
fi
rm -f "$PIDFILE" "$HBFILE"

# Release the manual hold on spira-lc.
spira-lc unhold "$ID" manual "hold-$ID" >/dev/null 2>&1 || true

printf 'released %s\n' "$ID"
