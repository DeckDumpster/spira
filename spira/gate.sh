#!/usr/bin/env bash
#
# gate.sh — the certification gate. Exits 0 if a branch may land.
#
#   gate.sh <branch> [repo-name]
#
# The gate is the Rust `gate` binary (gate/DESIGN.md, sp-0tpcs): it judges the branch MERGED
# onto its landing ref, and a branch that no longer merges is NO_VERDICT reason=conflict, not
# red. This file stays the one entry point every caller names (landing-pass, queue submit,
# batch.sh, gate-run.sh, the suites); it only resolves the binary and hands over.
#
# `exec -a "$0"` keeps this script's path in the process's argv, so the scans that look for a
# running gate by name (gate-run.sh unmanaged_gate, world.sh live_workers) still find it.
. "$(dirname "$0")/conf.sh" || exit 75
if [ ! -x "${SPIRA_GATE_BIN:-}" ]; then
    printf 'gate: the gate binary is not built (SPIRA_GATE_BIN=%s) — refusing to judge.\n' "${SPIRA_GATE_BIN:-<unset>}" >&2
    printf 'gate: VERDICT=NO_VERDICT reason=no-gate-bin branch=%s repo=%s suite=-\n' "${1:-?}" "${2:-?}" >&2
    exit 75
fi
exec -a "$0" "$SPIRA_GATE_BIN" --home "$(dirname "$0")" "$@"
