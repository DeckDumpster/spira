#!/usr/bin/env bash
# forge.sh — forge seam: open and query pull requests.
# SPIRA_FORGE may point to a substitute when a fixture replaces the real forge.
#
# pr-create <repo-dir> <head> <base> <title>   body on stdin; prints PR number
# pr-number <repo-dir> <head>                  prints the open PR number, or empty

set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib.sh
. "$HERE/lib.sh"

cmd="${1:-}"; shift
repo="${1:-}"; shift

case "$cmd" in
    pr-create)
        head="${1:-}" base="${2:-}" title="${3:-}"
        if ! ( cd "$repo" && ghq pr create --head "$head" --base "$base" \
                 --title "$title" --body-file - ) 2>/dev/null; then
            exit 1
        fi
        n="$( cd "$repo" && ghq pr view "$head" --json number -q .number 2>/dev/null )"
        printf '%s\n' "${n:-}"
        ;;
    pr-number)
        head="${1:-}"
        n="$( cd "$repo" && ghq pr view "$head" --json number -q .number 2>/dev/null )"
        printf '%s\n' "${n:-}"
        ;;
    *)
        printf 'forge.sh: unknown command: %s\n' "$cmd" >&2
        exit 1
        ;;
esac
