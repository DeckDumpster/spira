#!/usr/bin/env bash
#
# bare-main.sh — make a repository's main worktree bare while its linked worktrees stay working.
#
#   bare-main.sh check [repo]   exit 0 iff main is bare and every linked worktree is not
#   bare-main.sh apply [repo]   make it so, then check; idempotent
#
# core.bare=true in the shared config breaks every linked worktree, so it lives in the main
# worktree's config.worktree (extensions.worktreeConfig) and the shared value stays false.
# New linked worktrees read the shared value, so they need no repair.
set -uo pipefail

cmd="${1:-}"; repo="${2:-$(git rev-parse --git-common-dir 2>/dev/null || true)}"
case "$cmd" in check|apply) ;; *) echo "usage: bare-main.sh check|apply [repo]" >&2; exit 2 ;; esac
[ -n "$repo" ] || { echo "bare-main.sh: no repository given" >&2; exit 2; }

if [ -d "$repo/.git" ]; then common="$repo/.git"; else common="$repo"; fi
git --git-dir="$common" rev-parse --git-dir >/dev/null 2>&1 || { echo "bare-main.sh: $repo is not a repository" >&2; exit 2; }

check() {
  local rc=0 wt dir
  [ "$(git --git-dir="$common" rev-parse --is-bare-repository)" = true ] \
    || { echo "bare-main.sh: main worktree of $repo is not bare" >&2; rc=1; }
  for wt in "$common"/worktrees/*/; do
    [ -f "$wt/gitdir" ] || continue
    dir="$(dirname "$(cat "$wt/gitdir")")"
    [ -d "$dir" ] || continue
    [ "$(git -C "$dir" rev-parse --is-bare-repository 2>/dev/null)" = false ] \
      || { echo "bare-main.sh: linked worktree $dir is broken" >&2; rc=1; }
  done
  return $rc
}

if [ "$cmd" = check ]; then check; exit $?; fi

git --git-dir="$common" config extensions.worktreeConfig true || exit 1
git --git-dir="$common" config --file "$common/config.worktree" core.bare true || exit 1
git --git-dir="$common" config --file "$common/config" core.bare false || exit 1
check
