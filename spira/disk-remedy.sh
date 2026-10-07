#!/usr/bin/env bash
# disk-remedy.sh — the reconciler's Disk remedy (sp-lkfto.3): claw back space without
# asking, in the order recurrences have actually shown pays off (sp-q3lgs, sp-ln7f7,
# sp-1046x: unpruned podman images and abandoned worktrees, not one runaway file).
#
#   disk-remedy.sh
#
# 1. Reap every worktree whose bead is already closed. spira_destroy_worktree is the
#    harness's one destruction chokepoint and refuses on its own if the worktree is live or
#    dirty, so calling it unconditionally here is safe — the fence is already there.
# 2. podman image prune, dangling only (no -a: a tagged image still in use is never
#    guessed to be safe to remove).
# 3. podman volume prune, unattached only (podman's own default; never -a).
#
# Prints "REAPED <id> <path>" for each worktree removed. Whether this bought back enough
# space is not this script's problem to judge — the reconciler re-observes on its next
# pass and escalates to the Concierge if the floor is still breached.
#
# covers: spira/lib.sh spira/conf.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/conf.sh"
. "$HERE/lib.sh"

shopt -s nullglob
for w in "$SPIRA_RUN"/worktree/*/; do
    w="${w%/}"
    id="$(basename "$w")"
    [ "$(spira_bead_status "$id" 2>/dev/null)" = closed ] || continue
    repo_name="$(bead_repo "$id" 2>/dev/null)"
    repo_path="$(repo_root "$repo_name" 2>/dev/null)" || continue
    spira_destroy_worktree "$id" "$w" "$repo_path" \
        "reconciler: disk floor breached, reaping a finished worktree" \
        && printf 'REAPED %s %s\n' "$id" "$w"
done

if command -v podman >/dev/null 2>&1; then
    # batch-job: image prune walks the whole store
    podman image prune -f >/dev/null 2>&1
    # batch-job: volume prune walks the whole store
    podman volume prune -f >/dev/null 2>&1
fi
