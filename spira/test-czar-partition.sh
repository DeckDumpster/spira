#!/usr/bin/env bash
#
# test-czar-partition.sh — czar.fayth must claim only czar-trigger beads, not plan beads.
#
# THE DEFECT THIS GUARDS. A czar that claims plan beads becomes a second builder and stops
# watching the queue. The partition predicate must be FAYTH_LABELS matching ONLY
# SPIRA_CZAR_LABEL, not SPIRA_PLAN_LABEL or SPIRA_INCIDENT_LABEL.
#
# WHAT THIS SUITE CHECKS.
#   1. czar.fayth FAYTH_LABELS contains SPIRA_CZAR_LABEL (default: czar-trigger).
#   2. czar.fayth FAYTH_LABELS does NOT contain plan or incident.
#   3. When fayth_ready asks ready_count for czar, the include argument carries czar-trigger
#      and NOT plan (the end-to-end predicate check).
#   4. builder.fayth FAYTH_LABELS does NOT contain czar-trigger — partitions are disjoint.
#
# WHAT THIS SUITE DOES NOT USE. No bd, no systemd, no network. ready_count is stubbed.
#
# tier: T1
# covers: spira/chamber/czar.fayth spira/lib.sh spira/conf.sh
# hermetic-ok: no database, no systemd; ready_count and SPIRA_SUMMON are stubs
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"

. "$HERE/testlib.sh"
lack() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT INT TERM
mkdir -p "$T/run"

export SPIRA_HOME="$HERE"
export SPIRA_RUN="$T/run"
export SPIRA_CONF="$T/no-such.conf"
# shellcheck disable=SC1090
. "$HERE/lib.sh"

echo "test-czar-partition.sh"

# ==========================================================================================
echo
echo "czar.fayth — FAYTH_LABELS contains czar-trigger"
# ==========================================================================================
czar_labels="$(fayth_get czar FAYTH_LABELS)"
want "czar FAYTH_LABELS contains SPIRA_CZAR_LABEL (czar-trigger)" \
    "${SPIRA_CZAR_LABEL:-czar-trigger}" "$czar_labels"

# ==========================================================================================
echo
echo "czar.fayth — FAYTH_LABELS does not contain plan or incident (no partition bleed)"
# ==========================================================================================
lack "czar FAYTH_LABELS does not contain plan" "plan" "$czar_labels"
lack "czar FAYTH_LABELS does not contain incident" "incident" "$czar_labels"

# ==========================================================================================
echo
echo "fayth_ready czar — czar-trigger appears in the include argument to ready_count"
# ==========================================================================================
INCL_FILE="$T/observed-incl"
MOCK_READY=0
ready_count() {
    # $1 = include labels, $2 = exclude labels
    printf '%s' "$1" > "$INCL_FILE"
    printf '%d' "$MOCK_READY"
}
aeon_count() { printf '0'; }

fayth_ready czar >/dev/null 2>&1 || true
observed_incl="$(cat "$INCL_FILE" 2>/dev/null)"
want "fayth_ready czar passes czar-trigger in the include arg" \
    "${SPIRA_CZAR_LABEL:-czar-trigger}" "$observed_incl"
lack "fayth_ready czar does NOT pass plan in the include arg" "plan" "$observed_incl"

# ==========================================================================================
echo
echo "builder.fayth — FAYTH_LABELS does not contain czar-trigger (partitions are disjoint)"
# ==========================================================================================
# Proves the partitions cannot race on the same bead: a czar-trigger bead is not in the
# builder's predicate, so the czar has exclusive ownership of queue-state events.
builder_labels="$(fayth_get builder FAYTH_LABELS)"
lack "builder FAYTH_LABELS does not contain czar-trigger" \
    "${SPIRA_CZAR_LABEL:-czar-trigger}" "$builder_labels"

# ==========================================================================================
echo
echo "czar.fayth — FAYTH_LANE is set (draws from its own capacity, not the task pool)"
# ==========================================================================================
czar_lane="$(fayth_get czar FAYTH_LANE)"
want "czar FAYTH_LANE is set to czar" "czar" "$czar_lane"

echo
tl_summary
