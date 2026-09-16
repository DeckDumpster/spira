#!/usr/bin/env bash
# gate-touched.sh <base> <head> — the suites a branch adds or changes, one per line.
#
# Only suites present in the working tree are printed. In the gate's base trial that drops a
# suite the branch adds, so a new suite that is red is charged to the branch that added it.
set -uo pipefail
BASE="${1:?usage: gate-touched.sh <base> <head>}"
HEAD="${2:?usage: gate-touched.sh <base> <head>}"
repo="${SPIRA_GATE_REPO:-.}"
git -C "$repo" diff --name-only --diff-filter=AMR "$BASE...$HEAD" -- 'spira/test-*.sh' 2>/dev/null \
    | while IFS= read -r p; do [ -f "$p" ] && basename "$p"; done
exit 0
