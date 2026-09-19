#!/usr/bin/env bash
# gate-touched.sh <base> <head> — the suites a branch adds or changes, one per line.
#
# Only suites present in the working tree are printed. In the gate's base trial that drops a
# suite the branch adds, so a new suite that is red is charged to the branch that added it.
#
# SPIRA_GATE_EJECTED_SUITES  comma-separated names of suites that previously ejected this
#                              branch from the merge queue; always included so re-certification
#                              re-runs the suites that proved red (law-a-retry-must-change-an-input).
set -uo pipefail
BASE="${1:?usage: gate-touched.sh <base> <head>}"
HEAD="${2:?usage: gate-touched.sh <base> <head>}"
repo="${SPIRA_GATE_REPO:-.}"
{
    git -C "$repo" diff --name-only --diff-filter=AMR "$BASE...$HEAD" -- 'spira/test-*.sh' 2>/dev/null \
        | while IFS= read -r p; do [ -f "$p" ] && basename "$p"; done
    if [ -n "${SPIRA_GATE_EJECTED_SUITES:-}" ]; then
        printf '%s\n' "$SPIRA_GATE_EJECTED_SUITES" | tr ',' '\n' \
            | while IFS= read -r s; do [ -n "$s" ] && [ -f "spira/$s" ] && printf '%s\n' "$s"; done
    fi
} | sort -u
exit 0
