#!/usr/bin/env bash
# wiki-add-fence.sh — refuse blanket git-add forms targeting SPIRA_WIKI.
#
#   wiki-add-fence.sh
#
# Blanket staging on the wiki checkout sweeps another actor's uncommitted work
# into the commit (incident: sp-4fl2e). wiki-commit.sh is the canonical path;
# it stages each file explicitly. This fence keeps that pattern in force.
#
# Scans tracked .sh files for non-comment lines that pair SPIRA_WIKI with a
# blanket staging form: git add -A, git add ., or git commit -a.
# Exits 0 when none found, 1 when offenders are present, 3 when the tree
# cannot be examined (law-absence-needs-a-positive-control).
# covers: spira/aeon.sh spira/wiki-commit.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(git -C "$HERE" rev-parse --show-toplevel 2>/dev/null)"
[ -n "$ROOT" ] || ROOT="$(cd "$HERE/.." && pwd -P)"

count="$(git -C "$ROOT" ls-files -- '*.sh' 2>/dev/null | wc -l)"
if [ "${count:-0}" -lt 1 ]; then
    printf 'wiki-add-fence: refusing to report clean — no tracked .sh files found\n' >&2
    exit 3
fi

offenders=""
while IFS= read -r f; do
    # This file and its test reference these patterns intentionally.
    case "$f" in
        */wiki-add-fence.sh|wiki-add-fence.sh)           continue ;;
        */test-wiki-add-fence.sh|test-wiki-add-fence.sh) continue ;;
    esac
    # Find non-comment lines containing SPIRA_WIKI, then check for blanket forms.
    while IFS= read -r entry; do
        lineno="${entry%%:*}"
        text="${entry#*:}"
        if printf '%s\n' "$text" | grep -qE 'add -A|add \.[[:space:];|&]|add \.$|commit -a'; then
            offenders="${offenders}${f}:${lineno}: ${text}
"
        fi
    done < <(grep -n 'SPIRA_WIKI' "$ROOT/$f" 2>/dev/null | grep -v ':[[:space:]]*#')
done < <(git -C "$ROOT" ls-files -- '*.sh' 2>/dev/null)

if [ -z "$offenders" ]; then
    printf 'wiki-add-fence: no blanket-add forms targeting SPIRA_WIKI\n'
    exit 0
fi

printf 'wiki-add-fence: blanket git-add targeting SPIRA_WIKI:\n' >&2
printf '%s' "$offenders" | sed 's/^/wiki-add-fence:   /' >&2
printf 'wiki-add-fence: use wiki-commit.sh — it stages each file explicitly\n' >&2
exit 1
