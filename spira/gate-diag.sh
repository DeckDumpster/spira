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
# .github/workflows/gate.yml); it only resolves the binary and hands over — the same shim
# `gate.sh` uses for `gate` (sp-0tpcs).
#
# covers: spira/gate-diag.sh .github/workflows/gate.yml spira/testenv-batch.sh spira/tap-jsonl.sh
. "$(dirname "$0")/conf.sh" || exit 75
if [ ! -x "${SPIRA_GATE_DIAG_BIN:-}" ]; then
    printf 'gate-diag: the gate-diag binary is not built (SPIRA_GATE_DIAG_BIN=%s) — refusing to run.\n' "${SPIRA_GATE_DIAG_BIN:-<unset>}" >&2
    exit 75
fi
exec -a "$0" "$SPIRA_GATE_DIAG_BIN" --home "$(dirname "$0")" "$@"
