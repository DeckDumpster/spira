#!/usr/bin/env bash
#
# test-bare-main.sh — bare-main.sh makes the main worktree bare, refuses to populate it, and
# leaves existing and newly added linked worktrees able to commit.
#
# tier: T2
# covers: spira/bare-main.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t
R="$TMP/repo"
git init -q -b main "$R" && echo a >"$R/f" && git -C "$R" add f && git -C "$R" commit -qm a
git -C "$R" worktree add -q "$TMP/w1" -b b1

"$HERE/bare-main.sh" check "$R" 2>/dev/null; wantrc "check fails on a non-bare main" 1 $?
"$HERE/bare-main.sh" apply "$R" >/dev/null 2>&1; wantrc "apply succeeds" 0 $?
"$HERE/bare-main.sh" check "$R"; wantrc "check passes after apply" 0 $?
is "main is bare" true "$(git -C "$R" rev-parse --is-bare-repository)"
is "existing worktree is not bare" false "$(git -C "$TMP/w1" rev-parse --is-bare-repository)"

git -C "$TMP/w1" commit -q --allow-empty -m x; wantrc "existing worktree commits" 0 $?
git -C "$R" worktree add -q "$TMP/w2" -b b2; wantrc "worktree add works" 0 $?
git -C "$TMP/w2" commit -q --allow-empty -m y; wantrc "new worktree commits" 0 $?

rm -f "$R/f"
for c in "reset --hard" "checkout -f main" "merge b1"; do
  # shellcheck disable=SC2086
  out="$(git -C "$R" $c 2>&1)"; rc=$?
  [ "$rc" -ne 0 ] && ok "main refuses: git $c" || bad "main refuses: git $c" "$out"
done
[ ! -e "$R/f" ] && ok "main worktree stays empty" || bad "main worktree stays empty"

"$HERE/bare-main.sh" apply "$R" >/dev/null 2>&1; wantrc "apply is idempotent" 0 $?
rm -f "$TMP/w1/.git"
"$HERE/bare-main.sh" check "$R" 2>/dev/null; wantrc "check reports a broken worktree" 1 $?

tl_summary
