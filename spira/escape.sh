#!/usr/bin/env bash
#
# escape.sh — direct-summon an aeon for a named fayth, bypassing pool and lane checks.
#
#   escape.sh <fayth>           summon one aeon for <fayth>
#   escape.sh <fayth> --dry-run show what would be claimed, claim nothing
#
# WHEN TO USE. The sentinel decides how many aeons may start by comparing live aeon counts
# against pool and lane budgets. A bug in that arithmetic — wrong pool calculation, a lane
# accidentally counted against the pool — can prevent the bead that REPAIRS the bug from
# being claimed: the scheduler cannot schedule its own fix. This script is the path around
# that. It skips the pool and lane capacity checks entirely and invokes the aeon directly.
#
# THE CONTROL PLANE MUST NOT DEPEND ON THE DATA PLANE IT CONTROLS. Normal scheduling is
# the data plane; a broken scheduler is exactly when this — the control plane — must be
# reachable. Ryan's own case: eight P0s skipped because the bug fixing the ordering logic
# was itself subject to the ordering it would fix.
#
# WHAT THIS DOES NOT BYPASS. The fayth must exist. Its partition must have ready work.
# The account must have API capacity. The bead must not be poisoned. The fayth's own
# FAYTH_MAX_CONCURRENT is enforced by aeon.sh internally. All of that still applies.
#
# WHEN NOT TO USE. Normal scheduling is observable, coordinated, and lower-cost. Reserve
# this for a scheduler that is demonstrably failing to summon a fayth that has ready work.
set -uo pipefail
. "$(dirname "$0")/lib.sh"

FAYTH="${1:-}"
[ -n "$FAYTH" ] || die "usage: escape.sh <fayth> [--dry-run]"
DRY_FLAG="${2:-}"
F="$SPIRA_HOME/chamber/$FAYTH.fayth"
[ -f "$F" ] || die "no such fayth: $F"

# CAPACITY FIRST. An account outage is not a scheduling bug; spending a bead attempt on
# it is still wrong. aeon.sh checks this too, but checking here avoids the summon entirely.
if capacity_paused; then
    log "escape.sh $FAYTH: account out of capacity for another ${SPIRA_CAPACITY_LEFT}s — not summoning"
    exit 1
fi

r="$(fayth_ready "$FAYTH")" || die "$FAYTH: cannot read its partition"
if [ "${r:-0}" -eq 0 ]; then
    log "escape.sh $FAYTH: nothing ready in its partition — nothing to summon"
    exit 0
fi

log "escape.sh $FAYTH: $r ready — summoning directly (pool and lane checks bypassed)"
"${SPIRA_SUMMON:-systemd-run}" --user --collect --quiet \
    --unit="spira-aeon-$FAYTH-escape-$(date +%s)" \
    "--property=CPUQuota=${SPIRA_AEON_CPU_QUOTA:-70}%" --property=Nice=10 \
    --property=TimeoutStartSec="$(fayth_get "$FAYTH" FAYTH_TIMEOUT_SECONDS 3600)" \
    --setenv=PATH="$PATH" --setenv=HOME="$HOME" \
    "$SPIRA_HOME/aeon.sh" "$FAYTH" ${DRY_FLAG} 2>/dev/null
