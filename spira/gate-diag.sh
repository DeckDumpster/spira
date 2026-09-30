#!/usr/bin/env bash
# gate-diag.sh <results-root> — diagnostic output for a finished batch run.
#
# For each red or timeout suite: prints failing lines and the last N lines of
# its .out in a collapsible group (CI) or inline block (local); emits one
# ::error annotation per red suite when GITHUB_ACTIONS is set; prints a summary
# table to stdout; writes the same table to GITHUB_STEP_SUMMARY when that is set.
#
# The logic is the Rust `gate-diag` binary (gate-diag/DESIGN.md, sp-ubw2o): the same scan,
# the same two FAIL-line tiers, the same retry classification, the same red-suites.json and
# results.jsonl shapes. This file stays the one entry point every caller names (testenv,
# .github/workflows/gate.yml); it only hands over to the binary, by name on the launcher's PATH (sp-gypjk) — the same shim
# `gate.sh` uses for `gate` (sp-0tpcs).
#
# covers: spira/gate-diag.sh .github/workflows/gate.yml spira/testenv-batch.sh spira/tap-jsonl.sh
. "$(dirname "$0")/conf.sh" || exit 75
if ! command -v gate-diag >/dev/null 2>&1; then
    printf 'gate-diag: gate-diag is not on PATH (the launcher sets PATH to a release) — refusing to run.\n' >&2
    exit 75
fi
exec -a "$0" gate-diag --home "$(dirname "$0")" "$@"
