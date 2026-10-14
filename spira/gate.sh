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
# THE HARD CAP (Ryan, 2026-10-05, after a round's gate ran 2981 s to a red): a gate is 300 s of
# wall clock, whole, every phase included. Past it the gate is killed and the verdict is a named
# FAIL, never a wait. SPIRA_GATE_CAP_SECS may LOWER it (a suite proves the kill in seconds);
# nothing raises it — any other value is 300.
#
# gate.sh stays the parent rather than exec'ing, so it can name the over-cap verdict; the scans
# that find a running gate match this script's own path on its command line (spira-world
# live_workers, gate-run's unmanaged_gate), which the parent still carries. A signal to gate.sh
# is forwarded, so killing it still kills the gate.
GATE_CAP_SECS=300
case "${SPIRA_GATE_CAP_SECS:-}" in
    ''|*[!0-9]*) ;;
    *) [ "$SPIRA_GATE_CAP_SECS" -ge 1 ] && [ "$SPIRA_GATE_CAP_SECS" -lt 300 ] && GATE_CAP_SECS="$SPIRA_GATE_CAP_SECS" ;;
esac
timeout -k 10 "$GATE_CAP_SECS" gate --home "$(dirname "$0")" "$@" &
gate_pid=$!
trap 'kill -TERM "$gate_pid" 2>/dev/null' TERM INT HUP
rc=0
while :; do
    wait "$gate_pid"; rc=$?
    kill -0 "$gate_pid" 2>/dev/null || break
done
if [ "$rc" -eq 124 ] || [ "$rc" -eq 137 ]; then
    printf 'gate: VERDICT=FAIL reason=over-cap branch=%s repo=%s suite=- (killed at the %ss hard cap)\n' \
        "${1:-?}" "${2:-?}" "$GATE_CAP_SECS" >&2
    exit 1
fi
exit "$rc"
