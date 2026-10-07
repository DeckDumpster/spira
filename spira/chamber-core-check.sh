#!/usr/bin/env bash
# chamber-core-check.sh [chamber-dir] — every slug in every FAYTH_STATUTE_CORE must be a live memory.
# Exit 1 naming each unresolved slug; exit 2 when the store cannot be read (never a silent pass).
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
CHAMBER="${1:-$HERE/chamber}"
DB="${SPIRA_DB:?chamber-core-check: SPIRA_DB is not set}"

mem="$(timeout 5 bd -C "$DB" memories --json 2>/dev/null)" || { echo "chamber-core-check: cannot read memories at $DB" >&2; exit 2; }
keys="$(printf '%s' "$mem" | python3 -c 'import json,sys; print("\n".join(json.load(sys.stdin)))')" \
  || { echo "chamber-core-check: memories at $DB are not JSON" >&2; exit 2; }
[ -n "$keys" ] || { echo "chamber-core-check: $DB holds no memories — refusing to judge" >&2; exit 2; }

rc=0
for f in "$CHAMBER"/*.fayth; do
  [ -f "$f" ] || continue
  core="$(sed -n 's/^FAYTH_STATUTE_CORE=//p' "$f" | tail -1 | tr -d '"'"'")"
  IFS=',' read -r -a slugs <<<"$core"
  for slug in "${slugs[@]}"; do
    slug="${slug// /}"
    [ -n "$slug" ] || continue
    if ! grep -qxF -- "$slug" <<<"$keys"; then
      echo "chamber-core-check: $(basename "$f"): FAYTH_STATUTE_CORE names $slug, which is not a live memory" >&2
      rc=1
    fi
  done
done
exit "$rc"
