#!/usr/bin/env bash
# acceptance-agent.sh — deterministic stub for acceptance CI.
# Set SPIRA_AGENT to this path. Replaces claude in aeon.sh: drains stdin
# (the aeon prompt), commits acceptance-probe-<bead-id>.txt, finishes the bead with
# `work submit` exactly as the builder's {{FINISH}} brief tells a real model to, and
# prints the minimal JSON result aeon.sh expects.
#
# IT RUNS IN THE MODEL'S RESTRICTED ENVIRONMENT AND MAY USE NOTHING A MODEL CANNOT. Under
# lifecycle enforcement that environment has no bd, no SPIRA_DB and no release bin/ — PATH
# holds model-bin/ (only `work`, sp-zf4q3) and every bead operation is a `work` verb
# (sp-st0mm). The stub used to source conf.sh (which printed "spira-config not found on
# PATH" and resolved nothing) and `bd close` via SPIRA_DB; it died on "SPIRA_DB: unbound
# variable", so the probe bead cycled READY -> WORKING -> READY and never SUBMITTED
# (local acceptance phases A and D, 2026-10-05).
# covers: spira/aeon.sh
set -uo pipefail

cat >/dev/null  # drain the aeon prompt from stdin

# A SWEEP SESSION HAS NO BEAD. Ops and the other sweep personas summon the agent with no
# BEAD_ID; there is nothing to commit or close, so report a finished turn and succeed. Exiting
# 1 here left spira-ops FAILED and every later deploy's pre-health check refused on it.
if [ -z "${BEAD_ID:-}" ]; then
    printf '{"type":"result","subtype":"success","is_error":false,"duration_ms":100,"num_turns":1,"total_cost_usd":0}\n'
    exit 0
fi

# aeon.sh cd'd to the worktree before invoking us; PWD is the worktree.
# ONE FILE PER BEAD, WITH THE BEAD IN IT. A fixed empty acceptance-probe.txt was committed
# once, in phase A, and phase D's bead (same scratch repo, surviving state) then had nothing
# to commit — it closed on nothing and never landed.
printf '%s\n' "$BEAD_ID" > "acceptance-probe-${BEAD_ID}.txt"
git add "acceptance-probe-${BEAD_ID}.txt"
git commit -m "${BEAD_ID}: acceptance probe"

# The tip is read from this worktree's HEAD; the bead is the one `work` is bound to.
if ! work submit; then
    echo "acceptance-agent.sh: work submit failed for ${BEAD_ID}" >&2
    exit 1
fi

printf '{"type":"result","subtype":"success","is_error":false,"duration_ms":100,"num_turns":1,"total_cost_usd":0}\n'
