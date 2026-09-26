#!/usr/bin/env bash
#
# branch-sweep.sh — delete remote spira/* branches proven merged into base; report
# every branch that is not, and leave it alone.
#
#   branch-sweep.sh [repo-path] [--dry-run]
#
# One-time cleanup for the backlog of spira/<id> branches that accumulated on the
# remote before delete_branch_on_merge was turned on (sp-ggq1l). Most of that
# population landed by direct push, never through a merged pull request, so the
# setting does nothing to it — only a sweep does.
#
# ANCESTOR PROOF, NOT CONTENT-LANDED. spira_destroy_branch (lib.sh) asks
# content_landed so a caller that already vouched for a branch (the Sending, an
# empty-commit landing) is not refused for lacking a direct ancestor edge. This
# sweep has no such vouching caller behind it — a remote branch here may be a
# stale orphan nobody ever certified — so it asks the strictly narrower question
# and leaves anything it cannot prove, even a branch content_landed would allow.
#
# spira/queue/* IS NEVER TOUCHED HERE. It is not a bead branch; cockpit.sh reads
# that population for its stale-tip and unsent measurements, and verdict.sh
# already reaps the local half of it once a batch lands. Reported separately.
#
# aeon sessions have no forge credentials (GIT_SSH_COMMAND is neutered) and
# cannot push a delete; this script is for a context that has them.

# covers: spira/branch-sweep.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib.sh
. "$HERE/lib.sh"

REPO="${1:-${SPIRA_REPO:-}}"
DRY_RUN=0
for _a in "$@"; do [ "$_a" = "--dry-run" ] && DRY_RUN=1; done

[ -n "$REPO" ] && [ -e "$REPO/.git" ] || { echo "branch-sweep: no repository at '${REPO:-<empty>}'" >&2; exit 2; }

base="$(spira_landref "$REPO")" || { echo "branch-sweep: cannot resolve a base ref for $REPO" >&2; exit 2; }
remote="$(ref_remote "$base")" || { echo "branch-sweep: base '$base' has no remote; nothing to sweep" >&2; exit 2; }

swept=0
kept=0
excluded=0
failed=0

while IFS=' ' read -r fullref tip; do
    [ -n "$fullref" ] || continue
    br="${fullref#refs/remotes/$remote/}"
    case "$br" in
        spira/queue/*)
            excluded=$((excluded + 1))
            printf 'EXCLUDED      %s — queue population, not swept here\n' "$br"
            continue
            ;;
    esac
    if ! git -C "$REPO" merge-base --is-ancestor "$tip" "$base" 2>/dev/null; then
        kept=$((kept + 1))
        printf 'NOT-ANCESTOR  %s — leaving alone\n' "$br"
        continue
    fi
    if [ "$DRY_RUN" -eq 1 ]; then
        swept=$((swept + 1))
        printf 'WOULD-DELETE  %s (ancestor of %s)\n' "$br" "$base"
        continue
    fi
    if spira_git_push "$REPO" -q "$remote" --delete "$br" 2>/dev/null; then
        swept=$((swept + 1))
        printf 'DELETED       %s\n' "$br"
    else
        failed=$((failed + 1))
        printf 'FAILED        %s — push --delete did not succeed\n' "$br"
    fi
done < <(git -C "$REPO" for-each-ref --format='%(refname) %(objectname)' "refs/remotes/$remote/spira/")

printf 'branch-sweep: %d swept, %d not-ancestor (kept), %d excluded (queue), %d failed\n' \
    "$swept" "$kept" "$excluded" "$failed"
[ "$failed" -eq 0 ]
