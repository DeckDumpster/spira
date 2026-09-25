#!/usr/bin/env bash
#
# test-set-state-semantics.sh — bd set-state's own guarantee: single-valuedness and an
# event bead per call. Split out of test-set-state-writers.sh (UC-safety-fences-29): the
# lint half needs no database and stays T0; this half needs a real bd and stays T2. Folds
# into test-bd-contract.sh if that guard-seam suite is ever created — until then it is its
# own suite, per docs/test-plan/safety-fences.md's UC-29 verdict.
#
# THE FUNCTIONAL TEST calls set-state twice on the same dimension and asserts one label
# remains, not two, and that each call left an event bead. A test that passed on unfixed
# code with label add would have proved nothing about atomicity; this one would have shown
# two labels (the defect) where one was expected.
#
# tier: T2
# covers: spira/*.sh UC-safety-fences-29
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

. "$HERE/testdb.sh"
testdb_require test-set-state-semantics
trap 'testdb_drop' EXIT INT TERM
testdb_up setstatesem || bail "could not build fixture database"

echo "test-set-state-semantics.sh"

# ---------------------------------------------------------------------------
# Functional — single-valuedness.
# bd set-state removes the previous dim:val label before adding the new one.
# Two successive calls must leave exactly one label for the dimension.
# ---------------------------------------------------------------------------
echo
echo "set-state: two calls to the same dimension leave exactly one label:"
b="$(bd -C "$SPIRA_DB" create "set-state atomicity test" -l plan --silent 2>/dev/null | tr -d '[:space:]')"
[ -n "$b" ] || bail "fixture bead created: bd create returned nothing"
ok "fixture bead created ($b)"

bd -C "$SPIRA_DB" set-state "$b" "branch=feat/first"  --reason "first"  >/dev/null 2>&1
v1="$(bd -C "$SPIRA_DB" state "$b" branch 2>/dev/null)"
is "after first set-state: branch dimension is feat/first" "feat/first" "$v1"

bd -C "$SPIRA_DB" set-state "$b" "branch=feat/second" --reason "second" >/dev/null 2>&1
v2="$(bd -C "$SPIRA_DB" state "$b" branch 2>/dev/null)"
is "after second set-state: branch dimension is feat/second" "feat/second" "$v2"

branch_count="$(bd -C "$SPIRA_DB" show "$b" --json 2>/dev/null \
    | python3 -c 'import json,sys
d=json.load(sys.stdin); d=d if isinstance(d,list) else [d]
L=(d[0].get("labels") or []) if d else []
print(sum(1 for l in L if l.startswith("branch:")))')"
is "exactly one branch: label after two set-state calls" "1" "$branch_count"

# ---------------------------------------------------------------------------
# Functional — event trail.
# bd set-state creates an event bead per call; its id is <parent>.<n>.
# Two calls on bead $b produce sp-<suffix>.1 and sp-<suffix>.2.
# ---------------------------------------------------------------------------
echo
echo "set-state: each call creates an event bead:"
event_count="$(bd -C "$SPIRA_DB" list --all --json 2>/dev/null \
    | python3 -c "import json,sys
d=json.load(sys.stdin); d=d if isinstance(d,list) else [d]
parent='$b'
print(sum(1 for i in d if (i.get('id') or '').startswith(parent + '.')))")"
is "two set-state calls on $b leave two event beads" "2" "$event_count"

tl_summary
