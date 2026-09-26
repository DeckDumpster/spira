#!/usr/bin/env bash
#
# gh-intake-lint.sh — gh-intake.sh never constructs a write to GitHub and never reads a
# credential.
#
#   gh-intake-lint.sh              lint spira/gh-intake.sh; exit 1 naming each offense
#   gh-intake-lint.sh --scan FILE  lint one file; print line:text per hit; exit 0 either way
#
# THE PROPERTY (law-beads-is-never-public). The tracker is public and gh-intake.sh only
# reads it: a mutating curl flag (-X POST/PATCH/PUT/DELETE, --data) is a write GitHub cannot
# take back, and a credential reference (GITHUB_TOKEN, github.token, an Authorization
# header) is a token on the box the next caller can misuse. Both used to be asserted by
# grepping the source from inside test-gh-intake.sh — a T2 suite that stands up a stub
# curl/bd fixture to check two lines the fixture never exercises. This is that check,
# running once, without the fixture (UC-dispatch-06).
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
TARGET="$HERE/gh-intake.sh"

WRITE_PATTERN='curl[^|]*-X[[:space:]]*(POST|PATCH|PUT|DELETE)|--data|-d[[:space:]]'
CRED_PATTERN='GITHUB_TOKEN|github[.]token|Authorization:'
MATCH="(${WRITE_PATTERN})|(${CRED_PATTERN})"

# scan <file> -> line:text per hit, one pass; exit 0 either way. A comment line (leading #
# after stripping indentation) documents the property, it does not construct it.
scan() {
    local f="$1"
    grep -qE "$MATCH" "$f" 2>/dev/null || return 0
    awk -v PAT="$MATCH" '
        { stripped = $0; sub(/^[[:space:]]*/, "", stripped) }
        stripped ~ /^#/ { next }
        $0 ~ PAT { printf "%d:%s\n", NR, $0 }
    ' "$f"
}

case "${1:-}" in
--scan) scan "${2:?--scan needs a file}"; exit 0 ;;
esac

[ -r "$TARGET" ] || {
    printf 'gh-intake-lint: %s is missing — refusing to report clean\n' "$TARGET" >&2; exit 3; }

hits="$(scan "$TARGET")"
if [ -z "$hits" ]; then
    printf 'gh-intake-lint: clean — no write constructed, no credential read\n'
    exit 0
fi
cat >&2 <<'WHY'

REFUSED by gh-intake-lint.sh — the lines below construct a write to GitHub or read a
credential. gh-intake.sh must only read the tracker (law-beads-is-never-public):
WHY
while IFS=: read -r ln text; do
    printf 'gh-intake.sh:%s: %s\n' "$ln" "$(printf '%s' "$text" | sed 's/^[[:space:]]*//')" >&2
done <<< "$hits"
exit 1
