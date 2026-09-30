#!/usr/bin/env bash
#
# gate.sh — the certification gate. Exits 0 if a branch may land.
#
#   gate.sh <branch> [repo-name]
#
# The gate is the Rust `gate` binary (gate/DESIGN.md, sp-0tpcs): it judges the branch MERGED
# onto its landing ref, and a branch that no longer merges is NO_VERDICT reason=conflict, not
# red. This file stays the one entry point every caller names (landing-pass, queue submit,
# batch.sh, gate-run.sh, the suites); it only hands over to the binary, by name on the launcher's PATH (sp-gypjk).
#
# `exec -a "$0"` keeps this script's path in the process's argv, so the scans that look for a
# running gate by name (gate-run.sh unmanaged_gate, world.sh live_workers) still find it.
. "$(dirname "$0")/conf.sh" || exit 75
if ! command -v gate >/dev/null 2>&1; then
    printf 'gate: gate is not on PATH (the launcher sets PATH to a release) — refusing to judge.\n' >&2
    printf 'gate: VERDICT=NO_VERDICT reason=no-gate-bin branch=%s repo=%s suite=-\n' "${1:-?}" "${2:-?}" >&2
    exit 75
fi
exec -a "$0" gate --home "$(dirname "$0")" "$@"
