#!/usr/bin/env bash
#
# test-hermetic.sh — the gate judges the tree it is run in, not the installed copy.
#
#   ./test-hermetic.sh
#
# WHAT THIS SUITE IS FOR
# ----------------------
# gate-spira.sh finds its own location from its path (HERE) rather than from a
# configured variable, because the landing gate extracts a branch to a scratch
# worktree and runs that tree's gate command there.  If the gate stepped out to
# the installed copy instead, every branch would pass on the installed code — the
# one failure a gate must not have: it passes work it never looked at.
#
# This suite proves that property holds: an INSTALLED tree and a UNDER-TRIAL tree
# each announce which one they are, and running the gate from the under-trial tree
# with SPIRA_GATE_REPO pointing at the installed one must produce the under-trial
# marker, not the installed one.
#
# The main hermetic scan (systemctl / bd / git detection) was deleted in sp-1ctx
# because every suite now runs inside a container where those calls are legitimate.
# host-check.sh carries the host-escape declaration requirement that replaced it.
#
# defect: sp-4d8v
# covers: spira/gate-spira.sh spira/gate-fences.sh
# scar: gate-spira.sh used to cd to SPIRA_GATE_REPO rather than locating itself from HERE;
#   every branch passed on the installed copy's green. Surfaced only when a branch ADDED
#   a suite, because branches that only edit existing files are invisible to it.
# shellcheck disable=SC1090
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

echo "test-hermetic.sh"

[ "${XDG_RUNTIME_DIR:-}" = "/run/user/1001" ] || {
    printf 'SKIP test-hermetic.sh: not running inside testenv container\n' >&2
    exit 77
}

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
. "$HERE/gate-fences.sh"

# plant <root> <marker> — a tree whose fences announce which one it is.
# Every fence gate-spira.sh declares is stubbed; inventory.sh is overridden to
# print the marker and exit 1 so the gate stops there without running suites.
plant() {
    mkdir -p "$1/spira"
    cp "$HERE/gate-spira.sh" "$HERE/suite-covers.sh" "$1/spira/"
    # EVERY fence the gate insists on, derived from gate-spira.sh so adding a fence
    # here requires no edit in this suite (gate_fence_stubs fails closed if the list
    # is empty — law-absence-needs-a-positive-control).
    gate_fence_stubs "$HERE/gate-spira.sh" "$1/spira"
    # inventory.sh is the one that speaks: it runs after exclude.sh and before the suites, and
    # the gate prints its output and stops there. Override the stub gate_fence_stubs wrote.
    printf '#!/usr/bin/env bash\necho %s\nexit 1\n' "$2" > "$1/spira/inventory.sh"
}
plant "$TMP/under-trial" MARKER-UNDER-TRIAL
plant "$TMP/installed"   MARKER-INSTALLED

# THE CONTROL FIRST: the installed side must be able to produce its own marker, or "we did not
# see it" is a claim about a fixture that could never have spoken
# (law-absence-needs-a-positive-control).
want "the installed tree can announce itself" "MARKER-INSTALLED" \
     "$(cd "$TMP/installed" && env -i PATH="$PATH" HOME="$TMP" TERM=dumb \
        bash spira/gate-spira.sh 2>&1)"

out="$(cd "$TMP/under-trial" && env -i PATH="$PATH" HOME="$TMP" TERM=dumb \
       SPIRA_GATE_REPO="$TMP/installed" bash spira/gate-spira.sh 2>&1)"
want   "the gate judges the tree it was run in"      "MARKER-UNDER-TRIAL" "$out"
nowant "and not the checkout SPIRA_GATE_REPO names"  "MARKER-INSTALLED"   "$out"

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
