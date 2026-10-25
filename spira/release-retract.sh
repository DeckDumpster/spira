#!/usr/bin/env bash
#
# release-retract.sh — delete the DRAFT release and tag of a cut that did not publish.
# A published release is never touched.
#
# Usage: release-retract.sh <tag> [--repo <owner/repo>]
set -uo pipefail

_tag=""
_repo=()
while [ $# -gt 0 ]; do
    case "$1" in
        --repo)   _repo=(--repo "${2:-}"); shift 2 ;;
        --repo=*) _repo=(--repo "${1#--repo=}"); shift ;;
        -*)       printf 'release-retract: unknown option: %s\n' "$1" >&2; exit 2 ;;
        *)        _tag="$1"; shift ;;
    esac
done
[ -n "$_tag" ] || { printf 'usage: release-retract.sh <tag> [--repo <owner/repo>]\n' >&2; exit 2; }

_draft="$(gh release view "$_tag" "${_repo[@]}" --json isDraft --jq .isDraft 2>/dev/null || echo none)"
if [ "$_draft" != "true" ]; then
    printf 'release-retract: %s is not a draft (%s) — leaving it\n' "$_tag" "$_draft"
    exit 0
fi
gh release delete "$_tag" --yes "${_repo[@]}" 2>/dev/null || true
_slug="${_repo[1]:-${GITHUB_REPOSITORY:-}}"
[ -z "$_slug" ] || gh api --method DELETE "repos/$_slug/git/refs/tags/$_tag" 2>/dev/null || true
printf 'release-retract: retracted draft release and tag %s\n' "$_tag"
