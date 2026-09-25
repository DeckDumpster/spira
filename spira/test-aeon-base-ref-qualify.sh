#!/usr/bin/env bash
#
# test-aeon-base-ref-qualify.sh — worktree creation uses a fully-qualified
#   refs/remotes/... ref so a stray local refs/heads/origin/main cannot shadow it.
#
# THE DEFECT (sp-dd785). A local branch named refs/heads/origin/main existed alongside
# refs/remotes/origin/main. git treats any command given origin/main as ambiguous and
# exits fatal — including git worktree add in aeon.sh, which died with rc=255:
#
#   warning: refname 'origin/main' is ambiguous.
#   fatal: ambiguous object name: 'origin/main'
#
# aeon.sh uses qualify_base_ref() to resolve origin/main → refs/remotes/origin/main
# before passing it to any git command.
#
# CASES (law-absence-needs-a-positive-control):
#   1. Positive control: git worktree add with unqualified origin/main fails (rc != 0)
#      when both refs/heads/origin/main and refs/remotes/origin/main exist. Confirms the
#      test can detect the defect.
#   2. qualify_base_ref "origin/main" returns "refs/remotes/origin/main".
#   3. git worktree add with the qualified ref succeeds and the worktree HEAD is the
#      remote-tracking commit, not the stray local one.
#
# defect: sp-dd785
# covers: spira/aeon.sh spira/landing.sh spira/lib.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
nowant() { case "$3" in *"$2"*) bad "$1" "did not want [$2] in [$3]" ;; *) ok "$1" ;; esac; }

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

# ---- minimal harness copy to source lib.sh -----------------------------------------------
SPIRA_HOME="$TMP/spira"
mkdir -p "$SPIRA_HOME/chamber"
find "$HERE" -maxdepth 1 -name '*.sh' ! -name 'test-*.sh' -exec cp {} "$SPIRA_HOME/" \;
export SPIRA_RUN="$TMP/run"; mkdir -p "$SPIRA_RUN"
export SPIRA_CONF="$TMP/no-such.conf"
export SPIRA_DB="$TMP/no-such.db"
export SPIRA_REPO_MAP="$TMP/repo-map"; printf '' > "$SPIRA_REPO_MAP"

# GUARD ON THE HARNESS: if lib.sh fails to source, every assertion below is meaningless.
# shellcheck disable=SC1090
bash -c ". \"$SPIRA_HOME/lib.sh\"" \
    || { printf 'test-aeon-base-ref-qualify: fixture harness failed to source lib.sh\n' >&2; exit 1; }
# shellcheck disable=SC1090
. "$SPIRA_HOME/lib.sh"

# ---- git fixture -----------------------------------------------------------------------
REMOTE="$TMP/remote.git"
REPO="$TMP/repo"
git init -q --bare -b main "$REMOTE"
git init -q -b main "$REPO"
git -C "$REPO" config user.email t@t; git -C "$REPO" config user.name t
printf 'v1\n' > "$REPO/f"
git -C "$REPO" add f
git -C "$REPO" commit -q -m "initial"
git -C "$REPO" remote add origin "$REMOTE"
git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin
git -C "$REPO" symbolic-ref refs/remotes/origin/HEAD refs/remotes/origin/main

REMOTE_TRACKING_SHA="$(git -C "$REPO" rev-parse refs/remotes/origin/main)"

# Create a stray local branch refs/heads/origin/main at a DIFFERENT commit.
# This is the exact condition that caused sp-dd785.
printf 'stray\n' >> "$REPO/f"
git -C "$REPO" add f
git -C "$REPO" commit -q -m "stray commit"
STRAY_SHA="$(git -C "$REPO" rev-parse HEAD)"
git -C "$REPO" update-ref refs/heads/origin/main "$STRAY_SHA"  # stray local branch
git -C "$REPO" checkout -q main 2>/dev/null || true

[ "$REMOTE_TRACKING_SHA" != "$STRAY_SHA" ] \
    || { printf 'test-aeon-base-ref-qualify: fixture setup failed — SHAs not distinct\n' >&2; exit 1; }

echo
echo "test-aeon-base-ref-qualify.sh"
echo

# ===================================================================================
echo "CASE 1 (positive control): unqualified origin/main fails when stray ref exists:"
# ===================================================================================
# The defect: git treats origin/main as ambiguous when refs/heads/origin/main exists.
# This case confirms the test can detect the problem.
WORK1="$TMP/work1"; mkdir -p "$(dirname "$WORK1")"
err1="$(git -C "$REPO" worktree add -q -b spira/test1 "$WORK1" origin/main 2>&1)"; rc1=$?
git -C "$REPO" worktree remove -f "$WORK1" 2>/dev/null || true
git -C "$REPO" branch -D spira/test1 2>/dev/null || true
is "case 1: ambiguous origin/main causes git worktree add to fail" "1" "$([ "$rc1" -ne 0 ] && echo 1 || echo 0)"

echo
# ===================================================================================
echo "CASE 2: qualify_base_ref returns the fully-qualified remote-tracking ref:"
# ===================================================================================
got="$(qualify_base_ref "origin/main" "$REPO")"
is "case 2: qualify_base_ref origin/main → refs/remotes/origin/main" "refs/remotes/origin/main" "$got"

echo
# ===================================================================================
echo "CASE 3: worktree add with qualified ref succeeds and bases on the remote-tracking commit:"
# ===================================================================================
FQREF="$(qualify_base_ref "origin/main" "$REPO")"
WORK3="$TMP/work3"; mkdir -p "$(dirname "$WORK3")"
git -C "$REPO" worktree add -q -b spira/test3 "$WORK3" "$FQREF" 2>/dev/null; rc3=$?
is "case 3: worktree add with qualified ref succeeds" "0" "$rc3"
actual_sha="$(git -C "$WORK3" rev-parse HEAD 2>/dev/null || echo none)"
is "case 3: worktree HEAD is remote-tracking commit" "$REMOTE_TRACKING_SHA" "$actual_sha"
nowant "case 3: worktree HEAD is not the stray commit" "$STRAY_SHA" "$actual_sha"
git -C "$REPO" worktree remove -f "$WORK3" 2>/dev/null || true
git -C "$REPO" branch -D spira/test3 2>/dev/null || true

echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
