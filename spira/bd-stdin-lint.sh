#!/usr/bin/env bash
#
# bd-stdin-lint.sh — refuse a bare "-" body passed to `bd note` or `bd create -d`.
#
#   bd-stdin-lint.sh              scan spira/ and chamber/; exit 1 naming each offender
#   bd-stdin-lint.sh --scan FILE  scan one file; print line:text per hit; exit 0 either way
#
# THE PROPERTY. `bd note <id> - <<'EOF'` records the literal "-" and discards the heredoc
# body: bd note takes prose as positional arguments, so "-" is stored verbatim and exits 0.
# Likewise `bd create ... -d - <<'EOF'` — -d/--description is a plain string flag, not a
# stdin request. The correct forms are `bd note <id> --stdin <<EOF` and
# `bd create <title> --body-file - <<EOF`. This defect produced six beads with a dash where
# their body or notes should be (sp-j5z3).
#
# ONE PASS PER FILE, NOT ONE PER PATTERN. The two shapes are checked by a single regex
# alternation so each tracked file is read once; scanning twice — once per pattern, as the
# suite this replaced did over spira/ and chamber/ — pays the walk cost twice for the same
# answer (UC-dispatch-05).
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
ROOT="$(git -C "$HERE" rev-parse --show-toplevel 2>/dev/null)"
[ -n "$ROOT" ] || ROOT="$(cd "$HERE/.." && pwd -P)"

NOTE_PATTERN='bd[[:space:]].*note[[:space:]].*[[:space:]]-[[:space:]]<<'
CREATE_PATTERN='bd[[:space:]].*create[[:space:]].*(-d[[:space:]]+-|-d-|--description[[:space:]]+-)[[:space:]]*(<|$|[^-])'
MATCH="(${NOTE_PATTERN})|(${CREATE_PATTERN})"

# scan <file> -> line:text per hit, one pass; exit 0 either way. Comment lines (leading #,
# after stripping indentation) do not carry a live invocation — checked in the same awk
# pass as the match itself, not as a second grep -v over the first grep's output, which is
# exactly the double-read this fence exists to stop.
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

git -C "$ROOT" rev-parse --git-dir >/dev/null 2>&1 || {
    printf 'bd-stdin-lint: %s is not a git repository — nothing to scan\n' "$ROOT" >&2; exit 3; }

mapfile -t files < <(git -C "$ROOT" ls-files -- 'spira/*' 'chamber/*' 2>/dev/null)
[ "${#files[@]}" -gt 0 ] || {
    printf 'bd-stdin-lint: no spira/ or chamber/ files tracked — refusing to report clean\n' >&2; exit 3; }

bad=0
_offenders=()
for f in "${files[@]}"; do
    case "$f" in
        # THIS FENCE'S OWN TEST — content IS the planted example, not a live invocation.
        */test-bd-stdin.sh|test-bd-stdin.sh) continue ;;
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
    printf 'bd-stdin-lint: clean — %d file(s) checked\n' "${#files[@]}"
    exit 0
fi
cat >&2 <<'WHY'

REFUSED by bd-stdin-lint.sh — the lines above pass a bare "-" body to bd note or bd create,
which stores the literal "-" and discards the heredoc body that follows it.

Use the stdin forms instead:

  bd note   <id>    --stdin       <<'EOF'
  bd create <title> --body-file - <<'EOF'
WHY
printf '%s\n' "${_offenders[@]}"
exit 1
