#!/usr/bin/env bash
# acceptance-agent.sh — deterministic stub for acceptance CI.
# Set SPIRA_AGENT to this path. Replaces claude in aeon.sh: drains stdin
# (the aeon prompt), commits acceptance-probe-<bead-id>.txt, closes the bead, and
# prints the minimal JSON result aeon.sh expects.
# covers: spira/acceptance-run.sh spira/aeon.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"

cat >/dev/null  # drain the aeon prompt from stdin

# A SWEEP SESSION HAS NO BEAD. Ops and the other sweep personas summon the agent with no
# BEAD_ID; there is nothing to commit or close, so report a finished turn and succeed. Exiting
# 1 here left spira-ops FAILED and every later deploy's pre-health check refused on it.
if [ -z "${BEAD_ID:-}" ]; then
    printf '{"type":"result","subtype":"success","is_error":false,"duration_ms":100,"num_turns":1,"total_cost_usd":0}\n'
    exit 0
fi

# Source conf.sh to get SPIRA_DB (not exported by aeon.sh).
unset SPIRA_CONF_LOADED
. "$HERE/conf.sh"

# aeon.sh cd'd to the worktree before invoking us; PWD is the worktree.
# ONE FILE PER BEAD, WITH THE BEAD IN IT. A fixed empty acceptance-probe.txt was committed
# once, in phase A, and phase D's bead (same scratch repo, surviving state) then had nothing
# to commit — it closed on nothing and never landed.
printf '%s\n' "$BEAD_ID" > "acceptance-probe-${BEAD_ID}.txt"
git add "acceptance-probe-${BEAD_ID}.txt"
git commit -m "${BEAD_ID}: acceptance probe"

BD_IGNORE_SCHEMA_SKEW=1 "${SPIRA_BD:-bd}" -C "${SPIRA_DB}" close "${BEAD_ID}" \
    --reason "Acceptance stub: proves sentinel→summon→claim→commit→land." \
    >/dev/null 2>&1

printf '{"type":"result","subtype":"success","is_error":false,"duration_ms":100,"num_turns":1,"total_cost_usd":0}\n'
