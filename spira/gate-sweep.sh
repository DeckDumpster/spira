#!/usr/bin/env bash
#
# gate-sweep.sh — remove stale gate worktree registrations from a repository.
#
#   gate-sweep.sh <repo-path> [max-age-seconds]
#
# A gate worktree is stale when its path is under /tmp (the old default; $SPIRA_RUN is now
# preferred) OR when it is older than max-age-seconds and named .gate.<repo-basename>.
#
# Safe to run while gate.sh is running: the gate holds the worktree's lockfile, and the
# sweep skips any worktree whose lock it cannot immediately take — meaning an active gate
# is never interrupted.
#
# Called automatically from gate.sh before the flock, and safe to invoke by hand to clean
# up worktrees accumulated before gate.sh grew its own cleanup trap.
#
# ONLY TARGETS .gate.<repo-basename> WORKTREES. Other registrations — aeon worktrees, the
# main checkout, session scratchpads — are left alone. Non-gate orphans accumulated before
# this script existed must be removed by hand.

# covers: spira/gate.sh spira/gate-sweep.sh
set -uo pipefail
. "$(dirname "$0")/lib.sh"

REPO="${1:?usage: gate-sweep.sh <repo-path> [max-age-seconds]}"
MAX_AGE="${2:-${SPIRA_GATE_SWEEP_AGE:-3600}}"
REPO_BASE="$(basename "$REPO")"
TARGET_NAME=".gate.${REPO_BASE}"
removed=0

while IFS= read -r wt_path; do
    [ "$wt_path" = "$REPO" ] && continue
    # Match both old-style .gate.<repo> and new per-branch .gate.<repo>.<key>.
    case "$(basename "$wt_path")" in
        "${TARGET_NAME}"|"${TARGET_NAME}".*) ;;
        *) continue ;;
    esac

    # SKIP IF THE LOCK IS HELD — a running gate has exclusive access to its tree. The lock
    # file lives at <tree>.lock; flock -n -x acquires exclusively without waiting. If the
    # gate holds it, this exits non-zero and we move on. If the lock can be taken, the gate
    # is not running on this tree and removal is safe.
    lockfile="${wt_path}.lock"
    if [ -e "$lockfile" ] && ! flock -n -x "$lockfile" sh -c ":" 2>/dev/null; then
        printf 'gate-sweep: skipping %s — gate lock is held\n' "$wt_path" >&2
        continue
    fi

    stale=0
    # Old-style location: under /tmp but NOT inside the configured runtime directory.
    # In production SPIRA_RUN lives under /workspaces; this guard stops tests from
    # treating their own gate tree as stale when SPIRA_RUN happens to be under /tmp.
    case "$wt_path" in
        /tmp/*)
            case "$wt_path" in
                "${SPIRA_RUN}/"*) : ;;   # inside the runtime dir — not old-style
                *) stale=1 ;;
            esac
            ;;
    esac
    if [ "$stale" -eq 0 ] && [ -d "$wt_path" ]; then
        mtime="$(stat -c %Y "$wt_path" 2>/dev/null || echo 0)"
        age=$(( $(date +%s) - mtime ))
        [ "$age" -gt "$MAX_AGE" ] && stale=1
    fi
    [ "$stale" -eq 0 ] && continue

    printf 'gate-sweep: removing stale worktree %s\n' "$wt_path" >&2
    git -C "$REPO" worktree remove --force "$wt_path" 2>/dev/null \
        || rm -rf "$wt_path" 2>/dev/null \
        || true
    removed=$(( removed + 1 ))
done < <(git -C "$REPO" worktree list --porcelain 2>/dev/null | awk '/^worktree /{print $2}')

[ "$removed" -gt 0 ] && printf 'gate-sweep: removed %d stale gate worktree(s)\n' "$removed" >&2

# BATCH HOME SWEEP — remove /tmp/spira-batch-* fixture homes orphaned by SIGKILL.
# A home is stale when its container is absent from `podman ps` AND its mtime exceeds
# the suite timeout.  A home that is too recent is skipped even if the container is
# gone — it may belong to a batch that just started and has not yet written its owner
# file (law-bound-the-rare-path).
# SPIRA_PODMAN_PS_FILE: testing seam — a file whose lines are treated as running
# container names, replacing the live podman ps call.
_batch_timeout="${SPIRA_SUITE_TIMEOUT:-600}"
if [ -n "${SPIRA_PODMAN_PS_FILE:-}" ] && [ -f "${SPIRA_PODMAN_PS_FILE}" ]; then
    _live_containers="$(cat "${SPIRA_PODMAN_PS_FILE}" 2>/dev/null || true)"
else
    _live_containers="$(podman ps --format '{{.Names}}' 2>/dev/null || true)"
fi
for _bh in /tmp/spira-batch-*; do
    case "$_bh" in *.owner) continue ;; esac
    [ -e "$_bh" ] || continue
    _bh_name="$(basename "$_bh")"
    case "
${_live_containers}
" in
        *"
${_bh_name}
"*) continue ;;
    esac
    _bh_mtime="$(stat -c %Y "$_bh" 2>/dev/null || echo 0)"
    _bh_age=$(( $(date +%s) - _bh_mtime ))
    [ "$_bh_age" -le "$_batch_timeout" ] && continue
    printf 'gate-sweep: removing stale batch home %s (age %ds, container %s not running)\n' \
        "$_bh" "$_bh_age" "$_bh_name" >&2
    rm -rf "$_bh" 2>/dev/null || true
    removed=$((removed + 1))
done

[ "$removed" -gt 0 ] && printf 'gate-sweep: removed %d stale entry/entries total\n' "$removed" >&2
exit 0
