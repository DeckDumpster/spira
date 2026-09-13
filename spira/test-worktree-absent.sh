#!/usr/bin/env bash
#
# test-worktree-absent.sh — spira_destroy_worktree prunes a registry entry whose
# directory is gone, even when the path is outside SPIRA_RUN/worktree.
#
#   ./test-worktree-absent.sh
#
# THE BUG (sp-hl92). The reaper refused to remove a registered worktree whose path
# was outside the sanctioned root — the fence ran before the absent-directory check,
# so a /tmp-style gate worktree whose session had ended could never be cleared.
# sentinel.log accrued "was not removed" on every Sending pass, permanently, because
# the gap between "not under the root" and "directory does not exist" had no code path.
#
# THE FIX. The absent-directory check now runs before the fence. git worktree prune
# is a registry-only operation — it touches nothing on disk — so it is safe regardless
# of where the path points. The fence still guards the rm -rf path for existing paths.
#
# THE PROPERTIES UNDER TEST.
#
#   1. POSITIVE CONTROL. Register an out-of-root worktree, verify it is registered,
#      then verify spira_destroy_worktree returns 0 and the entry is gone.
#      Without this, silence from the function is indistinguishable from it being
#      pointed at a wrong fixture (law-absence-needs-a-positive-control).
#
#   2. A DIRECTORY THAT EXISTS outside the root is still REFUSED. This is the fence
#      the bead preserves: a misconfigured caller must not rm -rf an arbitrary path.
#
# NO DATABASE. spira_destroy_worktree does not call bd; it only touches the git
# registry and the filesystem. A git fixture with a real repo is the right dependency
# (law-prefer-the-real-dependency).
#
# defect: sp-hl92
# covers: spira/lib.sh
# scar: spira_destroy_worktree refused to prune a registry entry outside SPIRA_RUN/worktree even when the directory was gone; sentinel.log accrued "was not removed" on every pass permanently.
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()  { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }

T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT INT TERM
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

# Source lib.sh in a controlled environment with SPIRA_RUN and SPIRA_CONF pointing
# at our temp dir so no host configuration or pid files leak into the test results.
export SPIRA_RUN="$T/run"; mkdir -p "$SPIRA_RUN/worktree"
export SPIRA_CONF="$T/no-such.conf"
export SPIRA_REAPLOG="$T/reap.log"
# shellcheck disable=SC1090
. "$HERE/lib.sh"

# --------------------------------------------------------------------------------------
# GIT FIXTURE. A real repo with a bare remote so git worktree add works as it does
# in production; git worktree prune needs the worktree admin directory to exist.
# --------------------------------------------------------------------------------------
REMOTE="$T/remote.git"; REPO="$T/repo"
git init -q --bare -b main "$REMOTE"
git init -q -b main "$REPO"
git -C "$REPO" commit -q --allow-empty -m base
git -C "$REPO" remote add origin "$REMOTE"
git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin

echo "test-worktree-absent.sh"
echo

# ======================================================================================
echo "CASE 1 — absent out-of-root worktree: returns 0 and entry is pruned"
# ======================================================================================
# Register the worktree at a path OUTSIDE SPIRA_RUN/worktree (the /tmp-style path the
# landing gate used, whose session has ended and whose directory is now gone).
OUTSIDE_PATH="$T/outside/wt-test"
mkdir -p "$OUTSIDE_PATH"
git -C "$REPO" worktree add -q --detach "$OUTSIDE_PATH" HEAD

# Confirm it is registered before the test (positive control).
registered_before="$(git -C "$REPO" worktree list --porcelain 2>/dev/null \
    | awk -v p="$OUTSIDE_PATH" '/^worktree /{if($2==p)c++} END{print c+0}')"
is "out-of-root worktree is registered before removal" "1" "$registered_before"

# Now remove the directory, leaving only the registry entry — the exact state the bug
# produced: a path outside the root that no longer exists on disk.
rm -rf "$OUTSIDE_PATH"

spira_destroy_worktree "sp-test" "$OUTSIDE_PATH" "$REPO" "test-absent" 2>/dev/null
rc=$?
is "spira_destroy_worktree returns 0 for absent out-of-root path" "0" "$rc"

registered_after="$(git -C "$REPO" worktree list --porcelain 2>/dev/null \
    | awk -v p="$OUTSIDE_PATH" '/^worktree /{if($2==p)c++} END{print c+0}')"
is "registry entry is pruned after absent-path removal" "0" "$registered_after"

echo

# ======================================================================================
echo "CASE 2 — existing directory outside root: REFUSED (fence still holds)"
# ======================================================================================
# Register and keep a worktree whose directory still exists outside the root.
# spira_destroy_worktree must refuse and return 1.
LIVE_OUTSIDE="$T/outside/wt-live"
mkdir -p "$LIVE_OUTSIDE"
git -C "$REPO" worktree add -q --detach "$LIVE_OUTSIDE" HEAD

rc_live=0
spira_destroy_worktree "sp-test2" "$LIVE_OUTSIDE" "$REPO" "test-live" 2>/dev/null \
    || rc_live=$?
is "spira_destroy_worktree returns 1 for existing out-of-root path" "1" "$rc_live"

[ -d "$LIVE_OUTSIDE" ] \
    && ok  "directory still exists after refusal" \
    || bad "directory still exists after refusal" "directory was removed"

# Cleanup: remove the worktree the normal way.
git -C "$REPO" worktree remove --force "$LIVE_OUTSIDE" 2>/dev/null || true

echo

# ======================================================================================
echo "CASE 3 — REFUSED entry is logged"
# ======================================================================================
# The reap log must record the refusal so the operator can diagnose it.
log_entry="$(grep "sp-test2" "$SPIRA_REAPLOG" 2>/dev/null | grep REFUSED || true)"
[ -n "$log_entry" ] \
    && ok  "REFUSED is written to the reap log for an existing out-of-root path" \
    || bad "REFUSED is written to the reap log for an existing out-of-root path" "not found in $SPIRA_REAPLOG"

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
