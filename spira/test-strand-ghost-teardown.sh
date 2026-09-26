#!/usr/bin/env bash
#
# test-strand-ghost-teardown.sh — a bead whose aeon is mid-teardown (pidfile still present)
#   is NOT classified ghost even when its lease is past GHOST_GRACE.
#
#   ./test-strand-ghost-teardown.sh
#
# WHY THIS EXISTS. sp-nc74: aeon.sh's cleanup() removed the pidfile before the bead
# operations ran. When fixture_drop took longer than GHOST_GRACE (300s) after the last
# heartbeat, strand-classify.py saw: in_progress, lease expired, NO live holder — and
# ghost-reclaimed a bead whose aeon was demonstrably still alive. The fix moves the
# pidfile removal to after the bead operations, so holder_alive() stays true for the
# whole teardown.
#
# TWO CASES (law-absence-needs-a-positive-control):
#
#   1. POSITIVE CONTROL — pidfile removed (holder=0), expired lease → ghost IS classified.
#      Without this half a classifier that never fires looks correct.
#
#   2. MID-TEARDOWN — pidfile present (holder=1), expired lease → ghost NOT classified.
#      This is the case the fix enables: teardown took longer than GHOST_GRACE, but the
#      aeon is still running, so ghost must not fire.
#
# EXERCISED VIA FIXTURE, not a live database. strand-classify.py reads BEADS_FILE and
# READY_FILE; HOLDERS carries the liveness signal strand.sh builds from /proc+pidfiles.
#
# defect: sp-nc74
# tier: T1
# covers: spira/strand-classify.py spira/aeon.sh
# hermetic-ok: no database, no systemd, no network
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

# A lease_expires_at far enough in the past to exceed any GHOST_GRACE value.
EXPIRED="2000-01-01T00:00:00Z"

classify() {
    local beads_json="$1" holders="$2"
    printf '%s' "$beads_json" > "$TMP/beads.json"
    printf '[]' > "$TMP/ready.json"
    BEADS_FILE="$TMP/beads.json" \
    READY_FILE="$TMP/ready.json" \
    HOLDERS="$holders" LIVE=1 GHOST_GRACE=300 \
    SPIRA_ASK_LABEL=needs-operator \
        python3 "$HERE/strand-classify.py"
}

BEAD='[{"id":"sp-victim","title":"mid-teardown bead","status":"in_progress","labels":["spira","plan"],"lease_expires_at":"'"$EXPIRED"'"}]'

echo "test-strand-ghost-teardown.sh"

# ======================================================================================
echo
echo "case 1 — positive control: pidfile absent (holder=0) with expired lease IS ghost:"
# ======================================================================================
# Without this half, a classifier that never fires looks correct. The SAME fixture with
# holder=0 must produce a ghost row before we believe case 2 means anything.
out="$(classify "$BEAD" "sp-victim	0")"
want  "no-pidfile: ghost IS raised"   "ghost"     "$out"
want  "no-pidfile: sp-victim named"   "sp-victim" "$out"

# ======================================================================================
echo
echo "case 2 — mid-teardown: pidfile present (holder=1) with expired lease is NOT ghost:"
# ======================================================================================
# The aeon is still running (tearing down after a long fixture_drop). Even though the
# lease is past GHOST_GRACE, holder=1 means a live process holds the bead — ghost must
# not fire. This is the invariant the fix in aeon.sh enforces by keeping the pidfile
# alive until after the bead operations complete.
out="$(classify "$BEAD" "sp-victim	1")"
nowant "with-pidfile: ghost NOT raised" "ghost" "$out"
tl_summary
