#!/usr/bin/env bash
#
# binary-path-fence.sh — refuse a hardcoded binary path outside spira_bin.
#
#   binary-path-fence.sh              scan every tracked file; exit 1 naming each offender
#   binary-path-fence.sh --scan FILE  scan one file; print line:text per hit; exit 0 either way
#   binary-path-fence.sh --patterns   print the pattern list and exit
#
# THE PROPERTY (sp-zv7j4). conf.sh's spira_bin is the one resolver: a test run's only
# source of a binary is $SPIRA_ARTIFACTS, production's only source is the installed
# release's bin/, and neither falls back to the other. A script that spells out
# target/release/<bin>, target/debug/<bin> or bin/spira-<name> itself is a second,
# competing resolver — invisible until the tree it assumes (a --with-bins corpus, a stray
# cargo build) is not the one actually in front of it. That class of defect is exactly what
# sp-zv7j4 exists to remove: test-bead-file red only under --with-bins, 14 aeon fixtures
# that flipped behavior because work/spira-lc happened to exist, and a lifecycle switch
# that nearly went live in production from binary presence alone.
#
# WHAT IS EXEMPT — a short, named list, not a marker on every line: most of these files
# predate spira_bin and are the harness's actual build/release/test machinery, where a
# literal build-output path is the thing under test or the thing doing the building, not a
# second resolver competing with the first. binary-path-fence-allow, one repo-relative path
# per line, `#` comments.
#
# THE ESCAPE, for a single line in a file that is not wholesale exempt: a `# path-ok:
# <reason>` marker on the offending line or the one immediately above it — the same
# convention literal-lint.sh uses, so a reader who already knows that one reads this one
# for free.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
ROOT="$(git -C "$HERE" rev-parse --show-toplevel 2>/dev/null)"
[ -n "$ROOT" ] || ROOT="$(cd "$HERE/.." && pwd -P)"
ALLOW="${SPIRA_BINARY_PATH_FENCE_ALLOW:-$HERE/binary-path-fence-allow}"

patterns() {
    cat <<'PAT'
target/release/
target/debug/
target/aeon/
bin/spira-
PAT
}

OVERRIDE_MARKER="path-ok"

_pat="$(patterns | paste -sd'|' -)"

# scan <file> -> line:text per hit; exit 0 either way.
scan() {
    local f="$1"
    grep -qE "$_pat" "$f" 2>/dev/null || return 0
    awk -v PAT="$_pat" -v MARK="$OVERRIDE_MARKER" '
    { L[NR] = $0 }
    END {
        for (i = 1; i <= NR; i++) {
            line = L[i]
            if (line ~ MARK) continue
            if (i > 1 && L[i-1] ~ MARK) continue
            if (line ~ PAT) printf "%d:%s\n", i, line
        }
    }' "$f"
}

case "${1:-}" in
--patterns) patterns; exit 0 ;;
--scan)     scan "${2:?--scan needs a file}"; exit 0 ;;
esac

git -C "$ROOT" rev-parse --git-dir >/dev/null 2>&1 || {
    printf 'binary-path-fence: %s is not a git repository — nothing to scan\n' "$ROOT" >&2; exit 3; }

mapfile -t files < <(git -C "$ROOT" ls-files)
[ "${#files[@]}" -gt 0 ] || {
    printf 'binary-path-fence: nothing is tracked — refusing to report clean\n' >&2; exit 3; }

declare -A _allow
_allow["spira/binary-path-fence.sh"]=1
_allow["spira/binary-path-fence-allow"]=1
_allow["spira/test-binary-path-fence.sh"]=1
if [ -f "$ALLOW" ]; then
    while IFS= read -r _l; do
        _l="${_l%%#*}"
        _l="${_l#"${_l%%[![:space:]]*}"}"; _l="${_l%"${_l##*[![:space:]]}"}"
        [ -n "$_l" ] && _allow["$_l"]=1
    done < "$ALLOW"
fi

bad=0
_offenders=()
for f in "${files[@]}"; do
    [ -n "${_allow[$f]:-}" ] && continue
    # PROSE AND FIXTURE DATA ARE NOT CODE — no comment syntax to carry a path-ok marker,
    # and nothing here resolves a binary at runtime (same reasoning as literal-lint.sh).
    case "$f" in
        *.md|*.json|*.tsv) continue ;;
    esac
    [ -f "$ROOT/$f" ] || continue
    LC_ALL=C grep -qI . "$ROOT/$f" 2>/dev/null || continue
    hits="$(scan "$ROOT/$f")"
    [ -n "$hits" ] || continue
    bad=1
    while IFS=: read -r ln text; do
        _line="$(printf '%s:%s: %s' "$f" "$ln" "$(printf '%s' "$text" | sed 's/^[[:space:]]*//')")"
        printf '%s\n' "$_line"
        _offenders+=("$_line")
    done <<< "$hits"
done

if [ "$bad" = 0 ]; then
    printf 'binary-path-fence: clean — %d tracked file(s) contain no binary path outside spira_bin\n' "${#files[@]}"
    exit 0
fi
cat >&2 <<WHY

REFUSED by binary-path-fence.sh — the lines above name a binary's build-output path
directly instead of resolving it through conf.sh's spira_bin.

A test run's only source of a binary is \$SPIRA_ARTIFACTS; production's only source is the
installed release's bin/. A literal target/release/, target/debug/, target/aeon/ or
bin/spira-<name> path is a second resolver that silently assumes one tree or the other.

If the file genuinely IS the build/release machinery (add it to $ALLOW), or one line
genuinely has to (mark it \`# path-ok: <reason>\` on the line or the one above it).
WHY
printf '%s\n' "${_offenders[@]}"
exit 1
