#!/usr/bin/env bash
# test-git-identity.sh — rebase_branch and the landing merge supply their own committer identity
# so they succeed when the caller's environment carries none (systemd, env -i).
#
# covers: spira/lib.sh spira/landing.sh spira/conf.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()     { [ "$2" = "$3" ]     && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

EMPTYHOME="$TMP/emptyhome"; mkdir -p "$EMPTYHOME"
REPO="$TMP/repo"; RUN="$TMP/run"; REMOTE="$TMP/remote.git"
mkdir -p "$RUN/worktree"

git init -q --bare -b main "$REMOTE"
git init -q -b main "$REPO"
git -C "$REPO" remote add origin "$REMOTE"

GIT_AUTHOR_NAME=boot GIT_AUTHOR_EMAIL=boot@t GIT_COMMITTER_NAME=boot GIT_COMMITTER_EMAIL=boot@t \
    git -C "$REPO" commit -q --allow-empty -m base

git -C "$REPO" checkout -q -b spira/testbranch
GIT_AUTHOR_NAME=boot GIT_AUTHOR_EMAIL=boot@t GIT_COMMITTER_NAME=boot GIT_COMMITTER_EMAIL=boot@t \
    git -C "$REPO" commit -q --allow-empty -m "db-ucsl: work"

git -C "$REPO" checkout -q main
git -C "$REPO" checkout -q -b spira/mergebranch
GIT_AUTHOR_NAME=boot GIT_AUTHOR_EMAIL=boot@t GIT_COMMITTER_NAME=boot GIT_COMMITTER_EMAIL=boot@t \
    git -C "$REPO" commit -q --allow-empty -m "db-ucsl: merge-work"

git -C "$REPO" checkout -q main
GIT_AUTHOR_NAME=boot GIT_AUTHOR_EMAIL=boot@t GIT_COMMITTER_NAME=boot GIT_COMMITTER_EMAIL=boot@t \
    git -C "$REPO" commit -q --allow-empty -m "main: advance"

git -C "$REPO" push -q origin main spira/testbranch spira/mergebranch
git -C "$REPO" fetch -q origin

# --------------------------------------------------------------------------------------
# POSITIVE CONTROL. Bare rebase under env -i fails without identity — proves the test
# detects the problem before we trust the fix's passing result.
# --------------------------------------------------------------------------------------
CTRL_WT="$TMP/ctrl-wt"
git -C "$REPO" worktree add -q --detach "$CTRL_WT" spira/testbranch
ctrl_err="$(env -i HOME="$EMPTYHOME" PATH=/usr/bin:/bin \
    git -C "$CTRL_WT" rebase -q origin/main 2>&1 || true)"
git -C "$CTRL_WT" rebase --abort 2>/dev/null || true
want "positive-control: bare rebase without identity fails" "Committer identity unknown" "$ctrl_err"
git -C "$REPO" worktree remove --force "$CTRL_WT" 2>/dev/null || true

# --------------------------------------------------------------------------------------
# REBASE CASE. rebase_branch with SPIRA_GIT_NAME/EMAIL set to NON-DEFAULT values succeeds
# under HOME=$EMPTYHOME. Pinning to non-defaults means a hardcoded fallback using the
# default would be caught.
# --------------------------------------------------------------------------------------
TEST_NAME="db-ucsl-identity-test"
TEST_EMAIL="db-ucsl@test.invalid"

export SPIRA_HOME="$HERE"
export SPIRA_REPO="$REPO"
export SPIRA_RUN="$RUN"
export SPIRA_DB="$TMP/nodb"
export SPIRA_GIT_NAME="$TEST_NAME"
export SPIRA_GIT_EMAIL="$TEST_EMAIL"
export HOME="$EMPTYHOME"
unset GIT_AUTHOR_NAME GIT_AUTHOR_EMAIL GIT_COMMITTER_NAME GIT_COMMITTER_EMAIL 2>/dev/null || true

# shellcheck disable=SC1090
. "$HERE/lib.sh"

REBASE_FAILURE=""; REBASE_CONFLICTS=""; REBASE_REFUSED_REASON=""
rebase_branch "spira/testbranch" "origin/main" "$REPO"
rb_rc=$?
is  "rebase_branch returns 0"          0  "$rb_rc"
is  "REBASE_FAILURE empty on success"  "" "$REBASE_FAILURE"

committer="$(git -C "$REPO" log -1 --format='%cn <%ce>' spira/testbranch 2>/dev/null)"
is  "rebased commit carries configured committer" "$TEST_NAME <$TEST_EMAIL>" "$committer"

# --------------------------------------------------------------------------------------
# LANDING MERGE CASE. A non-fast-forward merge commit carries the configured identity
# when HOME has no git config and no GIT_COMMITTER_* are in the environment.
# --------------------------------------------------------------------------------------
LAND_WT="$TMP/land"
git -C "$REPO" worktree add -q --detach "$LAND_WT" origin/main
git -C "$LAND_WT" checkout -q -B landing origin/main
git -C "$LAND_WT" \
    -c "user.name=$TEST_NAME" -c "user.email=$TEST_EMAIL" \
    merge --no-edit -q -m "spira: land db-ucsl" "spira/mergebranch" >/dev/null 2>&1
land_rc=$?
is  "landing merge returns 0"  0  "$land_rc"

merge_committer="$(git -C "$LAND_WT" log -1 --format='%cn <%ce>' 2>/dev/null)"
is  "landing merge commit carries configured committer" "$TEST_NAME <$TEST_EMAIL>" "$merge_committer"

parent_count="$(git -C "$LAND_WT" cat-file -p HEAD 2>/dev/null | grep -c ^parent || echo 0)"
is  "landing merge is non-fast-forward (2 parents)" "2" "$parent_count"

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ] || exit 1
