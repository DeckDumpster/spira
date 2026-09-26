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

# _bead_state <id> -> its status, "(none)" when the store was read but the bead is not
# there, or "(unknown)" when the store itself could not be read. The two must never be
# confused: "(none)" is a bead that is confirmed gone, "(unknown)" is a read that failed and
# proves nothing (law-absence-needs-a-positive-control).
_bead_state() {
    bdjson show "$1" 2>/dev/null | python3 -c '
import sys, json
try: d = json.load(sys.stdin)
except Exception: print("(unknown)"); sys.exit()
d = d if isinstance(d, list) else [d]
s = d[0].get("status", "") if d else ""
print(s if s else "(none)")' 2>/dev/null
}

rc=0
while IFS= read -r branch; do
    [ -n "$branch" ] || continue
    bead_id="${branch#spira/}"
    state="$(_bead_state "$bead_id")"
    case "$state" in
        "(unknown)")
            printf 'holds.sh: %s: bead status unreadable\n' "$bead_id" >&2
            rc=1
            continue
            ;;
        "(none)"|closed) continue ;;
    esac
    touched="$(git -C "$REPO_PATH" diff --name-only "$BASE_REF...$branch" -- 2>/dev/null)" \
        || continue
    [ -n "$touched" ] || continue
    for p in "$@"; do
        grep -qxF "$p" <<<"$touched" && printf '%s\t%s\n' "$bead_id" "$p"
    done
done < <(git -C "$REPO_PATH" for-each-ref --format='%(refname:short)' 'refs/heads/spira/' 2>/dev/null | sort)

exit "$rc"
