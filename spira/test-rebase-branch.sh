#!/usr/bin/env bash
#
# test-rebase-branch.sh — rebase_branch's git-plumbing behavior against a real git
# repository: how it supplies a committer identity when none is ambient, and how a
# rebase failure is classified (no-branch, no-base, conflict, rebase-refused). Needs
# only lib.sh and a crafted git history — no landing.sh fixture, no testdb.
#
# The identity cases came from test-git-identity.sh, minus its "landing merge" half:
# that case asserted only what `git -c user.name=X merge` itself guarantees, never any
# code of landing.sh's own, so it was deleted rather than moved. The classification
# cases came from test-landing-rebase.sh, which never touched its repo-map/gate-stub/
# testdb fixture to run them.
#
# tier: T2
# covers: spira/lib.sh UC-landing-merge-queue-16
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

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
GIT_AUTHOR_NAME=boot GIT_AUTHOR_EMAIL=boot@t GIT_COMMITTER_NAME=boot GIT_COMMITTER_EMAIL=boot@t \
    git -C "$REPO" commit -q --allow-empty -m "main: advance"

git -C "$REPO" push -q origin main spira/testbranch
git -C "$REPO" fetch -q origin

echo "test-rebase-branch.sh"

# --------------------------------------------------------------------------------------
# POSITIVE CONTROL. Bare rebase under env -i fails without identity — proves the test
# detects the problem before we trust the fix's passing result.
# --------------------------------------------------------------------------------------
echo
CTRL_WT="$TMP/ctrl-wt"
git -C "$REPO" worktree add -q --detach "$CTRL_WT" spira/testbranch
ctrl_err="$(env -i HOME="$EMPTYHOME" PATH=/usr/bin:/bin GIT_CONFIG_NOSYSTEM=1 \
    git -C "$CTRL_WT" rebase -q origin/main 2>&1 || true)"
git -C "$CTRL_WT" rebase --abort 2>/dev/null || true
want "positive-control: bare rebase without identity fails" "Committer identity unknown" "$ctrl_err"
git -C "$REPO" worktree remove --force "$CTRL_WT" 2>/dev/null || true

# --------------------------------------------------------------------------------------
# THE IDENTITY FIX. rebase_branch with SPIRA_GIT_NAME/EMAIL set to NON-DEFAULT values
# succeeds under HOME=$EMPTYHOME, where the positive control above failed. Pinning to
# non-defaults means a hardcoded fallback using the default would be caught.
# --------------------------------------------------------------------------------------
TEST_NAME="db-ucsl-identity-test"
TEST_EMAIL="db-ucsl@test.invalid"

export SPIRA_HOME="$HERE"
export SPIRA_REPO="$REPO"
export SPIRA_RUN="$RUN"
export SPIRA_DB="$TMP/nodb"
export SPIRA_GIT_NAME="$TEST_NAME"
export SPIRA_GIT_EMAIL="$TEST_EMAIL"
export SPIRA_REPO_MAP="$TMP/no-map"
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

# Leave a clean, explicit environment for the classification cases below — SPIRA_GIT_NAME
# and SPIRA_GIT_EMAIL are pinned above to prove THIS section's fix; leaking them onward
# would leave the next section asserting against an environment nothing here declared.
unset SPIRA_GIT_NAME SPIRA_GIT_EMAIL
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

cat > "$TMP/repo-map" <<MAP
fixture-repo | $REPO | push | |
MAP

branch() {
    local id="$1" f="${2:-$1.txt}" c="${3:-$1}"
    git -C "$REPO" worktree add -q -b "spira/$id" "$RUN/worktree/$id" main
    printf '%s\n' "$c" > "$RUN/worktree/$id/$f"
    git -C "$RUN/worktree/$id" add -A
    git -C "$RUN/worktree/$id" commit -q -m "feat: $id — work"
}

drop_branch() {
    local id="$1"
    git -C "$REPO" worktree remove --force "$RUN/worktree/$id" >/dev/null 2>&1
    git -C "$REPO" branch -D "spira/$id" >/dev/null 2>&1
}

# --------------------------------------------------------------------------------------
# THE CLASSIFICATION BOTH GUARDS REST ON. rebase_branch returns 1 four ways and only one of
# them is a fact about the branch; before it said which, every caller that reopens on a
# rebase failure reopened for all four. Asserted directly, because the pass can only be
# steered into two of these and a guard reading a value nothing pins is a guard on a comment.
# --------------------------------------------------------------------------------------
echo
classify() {
    SPIRA_HOME="$HERE" SPIRA_RUN="$RUN" SPIRA_DB="$TMP/nodb" SPIRA_REPO="$REPO" \
    SPIRA_REPO_MAP="$TMP/repo-map" \
    bash -c '. "$1/lib.sh" >/dev/null 2>&1
             if rebase_branch "$2" "$3" "$4" fixture >/dev/null 2>&1
             then printf clean; else printf "%s" "${REBASE_FAILURE:-unset}"; fi' \
        _ "$HERE" "$1" "$2" "$REPO" 2>/dev/null
}
branch sp-kind
is "a ref that is not there is named no-branch" no-branch "$(classify spira/sp-nothere origin/main)"
is "a base that does not resolve is named no-base" no-base "$(classify spira/sp-kind refs/heads/no-such-base)"
is "a branch that rebases cleanly records no failure" clean "$(classify spira/sp-kind origin/main)"
drop_branch sp-kind

branch sp-kindclash shared.txt "from the branch"
printf '%s\n' "and the base disagrees" > "$REPO/shared.txt"
git -C "$REPO" add -A; git -C "$REPO" commit -q -m "base writes shared.txt again"
git -C "$REPO" push -q origin main; git -C "$REPO" fetch -q origin
is "and a real disagreement is named conflict" conflict "$(classify spira/sp-kindclash origin/main)"
drop_branch sp-kindclash

# THE REFUSED CASE: git declines to rebase (untracked file would be overwritten) without
# leaving any unmerged file. A non-conflict rebase failure must not reopen a finished bead,
# so it needs a name that is not "conflict". The fix sets REBASE_FAILURE=rebase-refused and
# captures git's first stderr line in REBASE_REFUSED_REASON.
classify_ext() {
    SPIRA_HOME="$HERE" SPIRA_RUN="$RUN" SPIRA_DB="$TMP/nodb" SPIRA_REPO="$REPO" \
    SPIRA_REPO_MAP="$TMP/repo-map" \
    bash -c '. "$1/lib.sh" >/dev/null 2>&1
             rebase_branch "$2" "$3" "$4" fixture >/dev/null 2>&1
             printf "%s|%s|%s" \
                 "${REBASE_FAILURE:-unset}" \
                 "${REBASE_REFUSED_REASON:-}" \
                 "${REBASE_CONFLICTS:-}"' \
        _ "$HERE" "$1" "$2" "$REPO" 2>/dev/null
}

branch sp-refused
printf 'base version\n' > "$REPO/blocked.txt"
git -C "$REPO" add blocked.txt
git -C "$REPO" commit -q -m "base adds blocked.txt"
git -C "$REPO" push -q origin main; git -C "$REPO" fetch -q origin
printf 'untracked\n' > "$RUN/worktree/sp-refused/blocked.txt"
_ext="$(classify_ext spira/sp-refused origin/main)"
_ext_fail="${_ext%%|*}"; _ext_rest="${_ext#*|}"; _ext_reason="${_ext_rest%%|*}"; _ext_conflicts="${_ext_rest#*|}"
is   "a rebase blocked by an untracked file is named rebase-refused" rebase-refused "$_ext_fail"
want "and git's refusal message is captured in REBASE_REFUSED_REASON" "untracked" "$_ext_reason"
is   "and REBASE_CONFLICTS is empty for a non-conflict failure" "" "$_ext_conflicts"
drop_branch sp-refused

branch sp-kindconflicts shared2.txt "from the branch"
printf '%s\n' "base disagrees" > "$REPO/shared2.txt"
git -C "$REPO" add -A; git -C "$REPO" commit -q -m "base writes shared2.txt"
git -C "$REPO" push -q origin main; git -C "$REPO" fetch -q origin
_ext="$(classify_ext spira/sp-kindconflicts origin/main)"
_ext_fail="${_ext%%|*}"; _ext_rest="${_ext#*|}"; _ext_conflicts="${_ext_rest#*|}"
is   "a real content conflict still produces REBASE_FAILURE=conflict"    conflict "$(classify spira/sp-kindconflicts origin/main)"
want "and REBASE_CONFLICTS names the colliding file"                      "shared2.txt" "$_ext_conflicts"
drop_branch sp-kindconflicts

# THE IDENTITY FIX, AGAIN, THIS TIME THROUGH CLASSIFICATION. The landing pass runs without
# an ambient git identity; git refuses any rebase that must replay a commit. The harness
# passes -c user.name/user.email from SPIRA_GIT_NAME and SPIRA_GIT_EMAIL, and a real
# conflict still classifies as conflict even with identity set.
rebase_id_classify() {
    local _home; _home="$(mktemp -d)"
    local _out
    _out="$(env -i \
        HOME="$_home" \
        PATH="$PATH" \
        SPIRA_HOME="$HERE" SPIRA_RUN="$RUN" SPIRA_DB="$_home/nodb" \
        SPIRA_REPO="$REPO" SPIRA_REPO_MAP="$TMP/repo-map" \
        SPIRA_GIT_NAME=testharness SPIRA_GIT_EMAIL=testharness@test.invalid \
        bash -c '. "$1/lib.sh" >/dev/null 2>&1
                 rebase_branch "$2" "$3" "$4" fixture >/dev/null 2>&1; _rc=$?
                 _cn="$(git -C "$4" log -1 --format=%cn "$2" 2>/dev/null)"
                 _ce="$(git -C "$4" log -1 --format=%ce "$2" 2>/dev/null)"
                 printf "%s|%s|%s|%s" "$_rc" "$_cn" "$_ce" "${REBASE_FAILURE:-}"' \
        _ "$HERE" "$1" "$2" "$REPO" 2>/dev/null)"
    rm -rf "$_home"
    printf '%s' "$_out"
}

branch sp-ident
printf 'base step\n' > "$REPO/ident-base.txt"
git -C "$REPO" add ident-base.txt
git -C "$REPO" commit -q -m "base adds ident-base.txt"
git -C "$REPO" push -q origin main; git -C "$REPO" fetch -q origin
_ri="$(rebase_id_classify spira/sp-ident origin/main)"
_ri_rc="${_ri%%|*}"; _ri_rest="${_ri#*|}"; _ri_cn="${_ri_rest%%|*}"; _ri_rest2="${_ri_rest#*|}"; _ri_ce="${_ri_rest2%%|*}"
is   "rebase succeeds in a clean env when SPIRA_GIT_NAME and SPIRA_GIT_EMAIL are set" "0" "$_ri_rc"
is   "the rebased commit's committer name is taken from SPIRA_GIT_NAME"  "testharness" "$_ri_cn"
is   "the rebased commit's committer email is taken from SPIRA_GIT_EMAIL" "testharness@test.invalid" "$_ri_ce"
drop_branch sp-ident

branch sp-ident-clash shared-ic.txt "branch content"
printf 'base content\n' > "$REPO/shared-ic.txt"
git -C "$REPO" add shared-ic.txt
git -C "$REPO" commit -q -m "base also writes shared-ic.txt"
git -C "$REPO" push -q origin main; git -C "$REPO" fetch -q origin
_ri_clash="$(rebase_id_classify spira/sp-ident-clash origin/main)"
_ri_clash_fail="${_ri_clash##*|}"
is "a real conflict returns REBASE_FAILURE=conflict even with identity set" "conflict" "$_ri_clash_fail"
drop_branch sp-ident-clash
tl_summary
