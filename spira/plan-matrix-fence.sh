#!/usr/bin/env bash
# plan-matrix-fence.sh <base> — the test plan's own gate fence: a suite deletion that orphans
# a use case's last cover is refused, and a stale coverage matrix is refused.
#
# Deliberately NARROWER than `plan-lint.sh` (no args): the whole-tree header lint
# (# tier:/# covers: on every suite, every declared UC id known) still reports real
# pre-existing gaps across suites that predate this epic's area pages and is not wired here —
# turning it into a hard gate failure today would fail every branch on debt this fence did not
# create. That flip is sp-94lbj's own scope, landing once the area pages are complete.
#
# WHAT THIS DOES FAIL, on every branch, regardless of what it touches:
#   1. plan-lint.sh --orphans <base>   — "deletion writes the plan"
#   2. plan-matrix.sh --check          — a stale docs/test-plan/coverage.json or COVERAGE.md
#
# Called from gate-touched.sh right after build-fence.sh, the one place in the repo-map's
# fence chain that runs regardless of SPIRA_GATE_SUITES — the same reasoning build-fence.sh
# documents: a check this cheap belongs before any suite runs, not gated behind one.
#
# tier: T0
# covers: spira/plan-matrix-fence.sh spira/gate-touched.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
BASE="${1:?usage: plan-matrix-fence.sh <base>}"

rc=0
if ! bash "$HERE/plan-lint.sh" --orphans "$BASE"; then
    printf 'plan-matrix-fence: a use case lost its last covering suite with no uncovered marker\n' >&2
    rc=1
fi
if ! bash "$HERE/plan-matrix.sh" --check; then
    rc=1
fi
exit "$rc"
