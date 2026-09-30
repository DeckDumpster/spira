#!/usr/bin/env bash
#
# gate-run.sh — run the landing gate detached and report its verdict in bounded slices.
#
#   gate-run.sh <branch> [repo-name]      start it if nothing is running, then wait up to
#                                         SPIRA_GATE_POLL seconds and report
#   gate-run.sh --status <branch> [repo]  answer now, waiting for nothing
#   gate-run.sh --exec   <branch> [repo]  the detached run itself; not for hand use
#
# The logic is the Rust `gate-run` binary (gate-run/DESIGN.md, sp-ubw2o): the same bounded-
# wait state machine, same state directory, same exit codes (0 pass, 1 fail, 2 still
# deciding, 3 no gate, 4 stale verdict, 5 died without a verdict). This file stays the one
# entry point every caller names (aeon, landing-pass, the suites); it only resolves the
# binary and hands over — the same shim `gate.sh` uses for `gate` (sp-0tpcs).
#
# `exec -a "$0"` keeps this script's path in the process's argv, so `alive()` (this crate's
# own liveness check on a detached run) and `lib.sh`/`cockpit.sh`'s generic `*gate*` scans
# still find it.
. "$(dirname "$0")/conf.sh" || exit 75
if ! command -v gate-run >/dev/null 2>&1; then
    printf 'gate-run: gate-run is not on PATH (the launcher sets PATH to a release) — refusing to run.\n' >&2
    exit 75
fi
exec -a "$0" gate-run --home "$(dirname "$0")" "$@"
