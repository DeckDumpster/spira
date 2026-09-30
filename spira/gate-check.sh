#!/usr/bin/env bash
#
# gate-check.sh — evaluate open gh:run gates and resolve those whose CI has passed.
#
#   gate-check.sh
#
# The logic is the Rust `gate-check` binary (gate-check/DESIGN.md, sp-ubw2o): the same five
# legs (discover, check, escalated-run resolution, flaky-suite beads, red-twice-suite beads,
# TSD ingest), the same dedup rules, the same `bd`/`gh`/`bead.sh`/`tsd-ingest.sh` calls. This
# file stays the one entry point every caller names — `spira-gate-check.service`'s
# `ExecStart`, and the suites that test the wiring — the same shim pattern `gate.sh` uses for
# `gate` (sp-0tpcs).
#
# CALLED FROM A TIMER, NOT THE SENTINEL. The sentinel's bespoke awaiting-ci sweep has been
# removed; this script is its replacement, on spira-gate-check.timer at a two-minute cadence.
#
# covers: spira/gate-check.sh spira/sentinel.sh spira/aeon.sh
. "$(dirname "$0")/conf.sh" || exit 75
if [ ! -x "${SPIRA_GATE_CHECK_BIN:-}" ]; then
    printf 'gate-check: the gate-check binary is not built (SPIRA_GATE_CHECK_BIN=%s) — refusing to run.\n' "${SPIRA_GATE_CHECK_BIN:-<unset>}" >&2
    exit 75
fi
exec -a "$0" "$SPIRA_GATE_CHECK_BIN" --home "$(dirname "$0")" "$@"
