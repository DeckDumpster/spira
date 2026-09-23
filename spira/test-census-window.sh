#!/usr/bin/env bash
# test-census-window.sh — landed() ignores SPIRA_VERDICT_WINDOW (full-history search)
#
#   ./test-census-window.sh
#
# covers: spira/lib.sh spira/census.sh
# hermetic-ok: no database, no systemd, no gh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok   — %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL — %s: %s\n' "$1" "$2"; }
is()  { [ "$2" = "$3" ] && ok "$1" || bad "$1" "expected [$2] got [$3]"; }

SPIRA_HOME="$HERE"; export SPIRA_HOME
. "$HERE/lib.sh"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

REMOTE="$TMP/remote.git"
REPO="$TMP/repo"
git init -q --bare "$REMOTE"
git -C "$REMOTE" symbolic-ref HEAD refs/heads/main
git clone -q "$REMOTE" "$REPO"
git -C "$REPO" config user.email "t@t"
git -C "$REPO" config user.name "t"

echo "test-census-window.sh"
echo
echo "1. landed() finds commit at depth > SPIRA_VERDICT_WINDOW (sp-census-suppression-starvation)"

TEST_ID="sp-census-window-test-$$"

GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t \
    git -C "$REPO" commit --allow-empty -q -m "base" 2>/dev/null
GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t \
    git -C "$REPO" commit --allow-empty -q -m "spira: land $TEST_ID" 2>/dev/null
for _i in 1 2 3; do
    GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t \
        git -C "$REPO" commit --allow-empty -q -m "noise $_i" 2>/dev/null
done
git -C "$REPO" push -q origin main >/dev/null 2>&1

# POSITIVE CONTROL (law-a-regression-test-must-be-seen-to-fail):
# Old landed() searched only the last SPIRA_VERDICT_WINDOW commits. With window=3 and
# the landing commit at depth 4, it returned 1 (not found) — wrong, suppresses the class.
# New landed() uses --grep with no -n limit; returns 0 regardless of depth.
_rc=1
SPIRA_VERDICT_WINDOW=3 landed "$TEST_ID" "$REPO" && _rc=0
is "landed() finds commit at depth 4 despite SPIRA_VERDICT_WINDOW=3" 0 "$_rc"

echo
printf '%s: %d passed, %d failed\n' "$(basename "$0")" "$pass" "$fail"
[ "$fail" -eq 0 ]
