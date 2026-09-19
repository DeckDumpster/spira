#!/usr/bin/env bash
# acceptance-agent.sh — deterministic stub for acceptance CI.
# Set SPIRA_AGENT to this path. Replaces claude in aeon.sh: drains stdin
# (the aeon prompt), commits acceptance-probe.txt, closes the bead, and
# prints the minimal JSON result aeon.sh expects.
# covers: spira/acceptance-run.sh spira/aeon.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"

cat >/dev/null  # drain the aeon prompt from stdin

[ -n "${BEAD_ID:-}" ] || { printf 'acceptance-agent: BEAD_ID not set\n' >&2; exit 1; }

# Source conf.sh to get SPIRA_DB (not exported by aeon.sh).
unset SPIRA_CONF_LOADED
. "$HERE/conf.sh"

# aeon.sh cd'd to the worktree before invoking us; PWD is the worktree.
printf '' > acceptance-probe.txt
git add acceptance-probe.txt
git commit -m "${BEAD_ID}: acceptance probe"

BD_IGNORE_SCHEMA_SKEW=1 "${SPIRA_BD:-bd}" -C "${SPIRA_DB}" close "${BEAD_ID}" \
    --reason "Acceptance stub: proves sentinel→summon→claim→commit→land." \
    >/dev/null 2>&1

printf '{"type":"result","subtype":"success","is_error":false,"duration_ms":100,"num_turns":1,"total_cost_usd":0}\n'
