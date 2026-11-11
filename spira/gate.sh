#!/usr/bin/env bash
#
# gate.sh — the certification gate. Exits 0 if a branch may land.
#
#   gate.sh <branch> [repo-name]
#
# The gate is the Rust `gate` binary (gate/DESIGN.md, sp-0tpcs): it judges the branch MERGED
# onto its landing ref, and a branch that no longer merges is FAIL reason=no-rebase, never
# NO_VERDICT. This file stays the one entry point every caller names (landing-pass, queue submit,
# batch.sh, gate-run.sh, the suites); it only hands over to the binary, by name on the launcher's PATH (sp-gypjk).
#
. "$(dirname "$0")/conf.sh" || exit 75
if ! command -v gate >/dev/null 2>&1; then
    printf 'gate: gate is not on PATH (the launcher sets PATH to a release) — refusing to judge.\n' >&2
    printf 'gate: VERDICT=NO_VERDICT reason=no-gate-bin branch=%s repo=%s suite=-\n' "${1:-?}" "${2:-?}" >&2
    exit 75
fi
# No wall clock bounds this process: the gate bounds its own tests (SPIRA_GATE_DEADLINE), ends
# a build that stops progressing (SPIRA_GATE_BUILD_STALL) and answers for both itself.
#
# gate.sh stays the parent rather than exec'ing: the scans that find a running gate match this
# script's own path on its command line (spira-world live_workers, gate-run's unmanaged_gate).
# A signal to gate.sh is forwarded, so killing it still kills the gate.
gate --home "$(dirname "$0")" "$@" &
gate_pid=$!
trap 'kill -TERM "$gate_pid" 2>/dev/null' TERM INT HUP
rc=0
while :; do
    wait "$gate_pid"; rc=$?
    kill -0 "$gate_pid" 2>/dev/null || break
done
exit "$rc"
