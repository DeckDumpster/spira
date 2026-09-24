#!/usr/bin/env bash
#
# testdb-mode-lint.sh — server-mode testdb requires a stated reason.
#
#   testdb-mode-lint.sh           scan every tracked spira/test-*.sh; exit 1 naming offenders
#   testdb-mode-lint.sh --scan FILE   scan one file; print line:text per hit; exit 0 either way
#
# THE PROPERTY. Embedded Dolt resets in ~5s; server mode pays a real dolt-beads-test
# round trip, median 110s. A suite that pins SPIRA_TESTDB_MODE=server without saying why
# is indistinguishable, at a glance, from one that needs the real engine — concurrency
# across processes, or SQL embedded mode refuses to run — and from one that was copied
# from a suite that did. 26 suites carried the pin; almost none carried the reason. This
# fence makes the two suites the request line must always mean the same thing: a header
# in the file, not a fact only the author remembers.
#
# WHAT IS REQUIRED. Any spira/test-*.sh that requests server mode — either
# `export SPIRA_TESTDB_MODE=server` or an inline `SPIRA_TESTDB_MODE=server` — must also
# carry a line matching:
#
#   # testdb-mode: server — <reason>
#
# anywhere in the file. The reason must be non-empty text after the em dash; the marker
# alone says nothing a reader could not already see from the request line itself.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
ROOT="$(git -C "$HERE" rev-parse --show-toplevel 2>/dev/null)"
[ -n "$ROOT" ] || ROOT="$(cd "$HERE/.." && pwd -P)"

REQUEST_PATTERN='(export[[:space:]]+)?SPIRA_TESTDB_MODE=server'
HEADER_PATTERN='^#[[:space:]]*testdb-mode:[[:space:]]*server[[:space:]]*—[[:space:]]*[^[:space:]]'

# requests_server <file> -> 0 if the file has a live request line (not a comment)
requests_server() {
    local f="$1"
    awk -v PAT="$REQUEST_PATTERN" '
        { stripped = $0; sub(/^[[:space:]]*/, "", stripped) }
        stripped ~ /^#/ { next }
        $0 ~ PAT { found = 1; exit }
        END { exit !found }
    ' "$f"
}

has_reason_header() {
    grep -qE "$HEADER_PATTERN" "$1" 2>/dev/null
}

# scan <file> -> the request line(s), only when the file has no reason header; exit 0 either way.
scan() {
    local f="$1"
    requests_server "$f" || return 0
    has_reason_header "$f" && return 0
    grep -nE "$REQUEST_PATTERN" "$f" 2>/dev/null
}

case "${1:-}" in
--scan) scan "${2:?--scan needs a file}"; exit 0 ;;
esac

git -C "$ROOT" rev-parse --git-dir >/dev/null 2>&1 || {
    printf 'testdb-mode-lint: %s is not a git repository — nothing to scan\n' "$ROOT" >&2; exit 3; }

mapfile -t files < <(git -C "$ROOT" ls-files -- 'spira/test-*.sh')
[ "${#files[@]}" -gt 0 ] || {
    printf 'testdb-mode-lint: no spira/test-*.sh tracked — refusing to report clean\n' >&2; exit 3; }

bad=0
_offenders=()
for f in "${files[@]}"; do
    case "$f" in
        # THIS FENCE'S OWN TEST — content IS the planted example, not a real request.
        */test-testdb-mode-lint.sh|test-testdb-mode-lint.sh) continue ;;
    esac
    [ -f "$ROOT/$f" ] || continue
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
    printf 'testdb-mode-lint: clean — %d tracked suite(s) checked\n' "${#files[@]}"
    exit 0
fi
cat >&2 <<'WHY'

REFUSED by testdb-mode-lint.sh — the suites above request server-mode testdb with no
stated reason.

Either flip to embedded (drop the SPIRA_TESTDB_MODE=server request — testdb_up defaults to
embedded when it is available) or say why the real engine is required:

    # testdb-mode: server — <reason>

anywhere in the file. The two genuine reasons: concurrency across processes that an
embedded, single-writer store cannot host, or server-only SQL (`bd sql` is refused in
embedded mode).
WHY
printf '%s\n' "${_offenders[@]}"
exit 1
