#!/usr/bin/env bash
#
# acceptance-prev-tag.sh — derive the prev-tag for acceptance's upgrade (B) and
# aged-install (D) phases: the newest spira-release-* tag, other than the one
# under test, that gh reports as published. release.yml creates every release
# as a draft, so tag order alone is not enough — a draft that never passed
# acceptance must never be picked as the predecessor to upgrade from.
#
# Usage: acceptance-prev-tag.sh <tag-under-test> [--repo <owner/repo>]
#
# Prints the derived tag on stdout, or nothing if no published predecessor
# exists (or gh is unreachable — silence is the safe default: an empty
# prev-tag makes acceptance-run.sh skip phases B/C/D rather than test a
# predecessor that was never accepted).
set -uo pipefail

_tag=""
_repo=""
while [ $# -gt 0 ]; do
    case "$1" in
        --repo)   _repo="${2:-}"; shift 2 ;;
        --repo=*) _repo="${1#--repo=}"; shift ;;
        -*)       printf 'acceptance-prev-tag: unknown option: %s\n' "$1" >&2; exit 2 ;;
        *)
            if [ -z "$_tag" ]; then _tag="$1"; else
                printf 'acceptance-prev-tag: too many positional arguments\n' >&2; exit 2
            fi
            shift ;;
    esac
done

[ -n "$_tag" ] || {
    printf 'usage: acceptance-prev-tag.sh <tag-under-test> [--repo <owner/repo>]\n' >&2
    exit 2
}

_gh_args=(release list --json tagName,isDraft)
[ -n "$_repo" ] && _gh_args=(--repo "$_repo" "${_gh_args[@]}")

_list="$(gh "${_gh_args[@]}" 2>/dev/null)" || _list=""
[ -n "$_list" ] || exit 0

printf '%s' "$_list" | python3 -c '
import json, sys
tag = sys.argv[1]
try:
    releases = json.load(sys.stdin)
except Exception:
    sys.exit(0)
pub = [r.get("tagName", "") for r in releases
       if not r.get("isDraft", True)
       and r.get("tagName", "").startswith("spira-release-")
       and r.get("tagName") != tag]
pub.sort()
print(pub[-1] if pub else "")
' "$_tag"
