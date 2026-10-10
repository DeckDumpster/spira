#!/usr/bin/env bash
# holds.sh — which OPEN beads have a branch, right now, that touches any of a set of paths.
#
#   holds.sh [--repo <name>] <path>...
#
# One line per hit, tab-separated: `<bead-id>\t<path>`. Empty output, exit 0, means nothing
# holds any of the given paths in the checked repo. Exit non-zero means the check could not
# be completed — a repo/landref that would not resolve, or a branch whose bead status could
# not be read — and must never be read as "nothing holds them"
# (law-a-control-that-cannot-check-must-refuse).
#
# Iterates existing `spira/*` branches, the same way held.sh does, rather than querying every
# open bead and deriving its branch name: a bead with no branch cut yet holds nothing by
# construction, and a branch whose bead was closed or deleted is skipped without failing the
# run — both fall out of walking branches that exist rather than beads that might.
#
# --repo defaults to the home repository (spira_home_repo); paths are relative to that
# repo's root, the same way `git diff --name-only` reports them.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/lib.sh"

_usage() { printf 'usage: holds.sh [--repo <name>] <path>...\n' >&2; exit 2; }

REPO_NAME=""
while [ $# -gt 0 ]; do
    case "$1" in
        --repo)     REPO_NAME="${2:?--repo needs a name}"; shift ;;
        --help|-h)  _usage ;;
        --)         shift; break ;;
        -*)         printf 'holds.sh: unknown flag %s\n' "$1" >&2; exit 2 ;;
        *)          break ;;
    esac
    shift
done
[ $# -ge 1 ] || _usage

[ -n "$REPO_NAME" ] || REPO_NAME="$(spira_home_repo)"
REPO_PATH="$(repo_root "$REPO_NAME" 2>/dev/null)" \
    || { printf 'holds.sh: repo %s not in map\n' "$REPO_NAME" >&2; exit 1; }
[ -e "$REPO_PATH/.git" ] \
    || { printf 'holds.sh: %s has no .git\n' "$REPO_PATH" >&2; exit 1; }
BASE_REF="$(spira_landref "$REPO_PATH" 2>/dev/null)" \
    || { printf 'holds.sh: could not resolve landref for %s\n' "$REPO_NAME" >&2; exit 1; }

# _bead_state <id> -> its lifecycle state, "(none)" when the machine was read but has no row
# for the bead, or "(unknown)" when the machine itself could not be read. The two must never
# be confused: "(none)" is a bead that is confirmed gone, "(unknown)" is a read that failed
# and proves nothing (law-absence-needs-a-positive-control).
_bead_state() {
    local s rc
    s="$(spira-lc state "$1" 2>/dev/null)"; rc=$?
    case "$rc" in
        0) printf '%s\n' "$s" ;;
        1) printf '(none)\n' ;;
        *) printf '(unknown)\n' ;;
    esac
}

rc=0
while IFS= read -r branch; do
    [ -n "$branch" ] || continue
    bead_id="${branch#spira/}"
    state="$(_bead_state "$bead_id")"
    case "$state" in
        "(unknown)")
            printf 'holds.sh: %s: bead state unreadable\n' "$bead_id" >&2
            rc=1
            continue
            ;;
        "(none)"|LANDED|SUPERSEDED|DROPPED|DONE) continue ;;
    esac
    touched="$(git -C "$REPO_PATH" diff --name-only "$BASE_REF...$branch" -- 2>/dev/null)" \
        || continue
    [ -n "$touched" ] || continue
    for p in "$@"; do
        grep -qxF "$p" <<<"$touched" && printf '%s\t%s\n' "$bead_id" "$p"
    done
done < <(git -C "$REPO_PATH" for-each-ref --format='%(refname:short)' 'refs/heads/spira/' 2>/dev/null | sort)

exit "$rc"
