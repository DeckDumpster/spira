#!/usr/bin/env bash
#
# test-landed-search.sh — lib.sh:landed()'s commit search, tested directly against a real
# git repository (no bd, no sentinel, no CHECK 5 fixture).
#
# Absorbs test-census-window.sh (SPIRA_VERDICT_WINDOW is ignored) and the search-only cases
# of test-check5-body-search.sh / test-landed-stays-landed.sh (body-only match, depth with
# no window) — landed() is the one implementation both `landed <id> <repo>` callers and
# CHECK 5's inlined walk are built from, so proving the property here proves it for both.
# The CHECK 5-level positive controls (a bare closed bead reopened by a real sentinel pass)
# stay in the CHECK 5 suites; those exercise the caller, not the search.
#
# The deep-history fixture is built with one `git fast-import` process, not 401 spawned
# `git commit` processes — it proves "no depth window" with a genuinely deep commit, not
# with SPIRA_VERDICT_WINDOW, which landed() never reads at all (see case 1).
#
# defect: sp-d9x93 sp-796o sp-m0s7 sp-a9g sp-37q
# tier: T2
# covers: spira/lib.sh UC-landed-audit-reaping-01 UC-landed-audit-reaping-02 UC-landed-audit-reaping-03
# hermetic-ok: no database, no systemd, no gh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok   — %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL — %s: %s\n' "$1" "$2"; }
is()  { [ "$2" = "$3" ] && ok "$1" || bad "$1" "expected [$2] got [$3]"; }

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM

# EXPLICIT, MINIMAL ENVIRONMENT: a real spira/repo-map on this box must never decide this
# suite's verdict — landed() must resolve every fixture repo's base from its own git remote.
SPIRA_HOME="$HERE"; SPIRA_REPO_MAP="$TMP/no-such-repo-map"; export SPIRA_HOME SPIRA_REPO_MAP
# shellcheck disable=SC1090
. "$HERE/lib.sh"

REMOTE="$TMP/remote.git"
REPO="$TMP/repo"
git init -q --bare -b main "$REMOTE"
git init -q -b main "$REPO"
git -C "$REPO" config user.email "t@t"
git -C "$REPO" config user.name "t"
git -C "$REPO" remote add origin "$REMOTE"
git -C "$REPO" commit -q --allow-empty -m base
git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin
git -C "$REPO" remote set-head origin main

# build_deep_history <subject> <pad-count> — extend $REPO/main with one commit carrying
# <subject>, then <pad-count> more commits after it, all via a single fast-import stream.
# Echoes the landing commit's SHA.
build_deep_history() {
    local subject="$1" n="$2" i
    {
        printf 'commit refs/heads/main\n'
        printf 'committer t <t@t> 1700000000 +0000\n'
        printf 'data <<EOF_MSG\n%s\nEOF_MSG\n' "$subject"
        printf 'from refs/heads/main^0\n'
        for ((i = 1; i <= n; i++)); do
            printf 'commit refs/heads/main\n'
            printf 'committer t <t@t> %d +0000\n' $((1700000000 + i))
            printf 'data <<EOF_MSG\npad %d\nEOF_MSG\n' "$i"
        done
    } | git -C "$REPO" fast-import --quiet
    git -C "$REPO" rev-parse "main~$n"
}

echo "test-landed-search.sh"

# ======================================================================================
echo
echo "1. landed() has no depth window (UC-01; sp-d9x93, sp-37q):"
# POSITIVE CONTROL (law-a-regression-test-must-be-seen-to-fail): a windowed search would
# miss a commit 500 back. Old landed() also honoured SPIRA_VERDICT_WINDOW as an -n limit;
# the current one never reads it — asserted directly, not inferred from the depth alone.
# ======================================================================================
DEEP_ID="sp-deep-search-test"
land_sha="$(build_deep_history "spira: land $DEEP_ID" 500)"
git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin

if git -C "$REPO" merge-base --is-ancestor "$land_sha" origin/main 2>/dev/null; then
    ok "fixture: landing commit is on origin/main, 500 commits back"
else
    bad "fixture: landing commit is on origin/main" "not an ancestor — fixture is wrong"
fi

_rc=1
landed "$DEEP_ID" "$REPO" && _rc=0
is "landed() finds a commit 500 deep with no window" 0 "$_rc"

_rc=1
SPIRA_VERDICT_WINDOW=1 landed "$DEEP_ID" "$REPO" && _rc=0
is "landed() ignores SPIRA_VERDICT_WINDOW entirely" 0 "$_rc"

_rc=0
landed sp-not-landed-anywhere "$REPO" || _rc=$?
is "landed() returns 1 (not 0) for an id no commit names" 1 "$_rc"

# ======================================================================================
echo
echo "2. landed() searches the full message, not just the subject (UC-01; sp-m0s7):"
# ======================================================================================
BODY_ID="sp-body-search-test"
git -C "$REPO" commit -q --allow-empty -m "$(printf 'refactor: cleanup\n\nThis resolves %s — the underlying work was\nalready applied in a prior squash.' "$BODY_ID")"
git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin

_rc=1
landed "$BODY_ID" "$REPO" && _rc=0
is "landed() finds a body-only reference" 0 "$_rc"

# ======================================================================================
echo
echo "3. KNOWN DEFECT — prefix collision (UC-02; gap, no fix here):"
# landed() matches via 'git log --grep=\$id', an unanchored substring/regex search. A
# commit naming sp-prefix-collision-testXX satisfies a query for sp-prefix-collision-test,
# so a closed-but-unlanded bead whose id is a prefix of a landed one is never reopened.
# This is exactly the failure law-closed-is-not-landed exists to catch.
#
# This test documents the CURRENT (defective) behaviour rather than silently fixing the
# matcher: a test-plan bead may not carry a product fix (see docs/test-plan/
# landed-audit-reaping.md, gap 1). Fix tracked as sp-ogogs; update this case to assert the
# corrected behaviour once it lands, rather than deleting it.
# ======================================================================================
git -C "$REPO" commit -q --allow-empty -m "sp-prefix-collision-testXX: unrelated work"
git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin

_rc=1
landed sp-prefix-collision-test "$REPO" && _rc=0
is "KNOWN DEFECT: a longer id's commit satisfies its own prefix (see the tracking bead)" 0 "$_rc"

# ======================================================================================
echo
echo "4. landed() answers 'cannot tell', not 'not landed', when the base is unresolvable (UC-03):"
# FAIL-CLOSED: reading an unresolvable base as "no commit names it" would make every closed
# bead in a ref-less repository look unlanded. rc 2 is reserved for exactly this.
# ======================================================================================
NOGIT="$TMP/not-a-repo"
mkdir -p "$NOGIT"

_rc=0
landed sp-anything "$NOGIT" || _rc=$?
is "landed() returns 2 when the repo has no .git at all" 2 "$_rc"

echo
printf '%s: %d passed, %d failed\n' "$(basename "$0")" "$pass" "$fail"
[ "$fail" -eq 0 ]
