#!/usr/bin/env bash
#
# stale-drafts.sh — list spira-release-* and spira-N.N.N draft releases older than a day and exit 1 if
# there are any. A cut that did not publish retracts its own draft; one left behind
# sorts above every published release for maintainers.
#
# Usage: stale-drafts.sh [--repo <owner/repo>] [--max-age-hours N]
set -uo pipefail

_repo=()
_hours=24
while [ $# -gt 0 ]; do
    case "$1" in
        --repo)          _repo=(--repo "${2:-}"); shift 2 ;;
        --max-age-hours) _hours="${2:-}"; shift 2 ;;
        *) printf 'stale-drafts: unknown argument: %s\n' "$1" >&2; exit 2 ;;
    esac
done

_rows="$(timeout 5 gh release list "${_repo[@]}" --limit 1000 --json tagName,isDraft,createdAt \
    --jq '.[] | select(.isDraft) | [.tagName, .createdAt] | @tsv')" || {
    printf 'stale-drafts: gh release list failed\n' >&2; exit 2; }
_cutoff="$(date -u -d "-${_hours} hours" +%Y-%m-%dT%H:%M:%SZ)"
_stale="$(printf '%s\n' "$_rows" | awk -F'\t' -v c="$_cutoff" '$1 ~ /^spira-(release-|[0-9])/ && $2 < c {print $1}')"
[ -z "$_stale" ] && exit 0
printf '%s\n' "$_stale"
printf 'stale-drafts: %s draft release(s) older than %sh\n' "$(printf '%s\n' "$_stale" | wc -l)" "$_hours" >&2
exit 1
