#!/usr/bin/env bash
#
# suite-state-fence.sh — refuse a landing when spira/suite-state is malformed,
# names a non-existent suite, or keeps a quarantine alive on a CLOSED bead.
#
# A closed bead is nobody's work. suites.sh hygiene requires land_state:LANDED to
# reactivate a quarantine; CLOSED is never LANDED, so the check is permanently
# off with nobody accountable for it.
#
# Exits 0 when clean, 1 when any check fails.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/suite-state.sh"

STATE_FILE="$(suite_state_file "$HERE/..")"
if [ ! -r "$STATE_FILE" ]; then
    printf 'suite-state-fence: no state file — nothing to lint\n'
    exit 0
fi

rc=0
suite_state_lint "$STATE_FILE" "$HERE" || rc=1

# Derive SPIRA_DB from conf.sh if not already in the environment.
[ -n "${SPIRA_DB:-}" ] || . "$HERE/lib.sh" 2>/dev/null || true

BDCMD="${SPIRA_BD:-bd}"
while IFS=$'\t' read -r suite state _since bead _reason; do
    [ "$state" = "quarantined" ] || continue
    [ -n "$bead" ] || continue
    if [ -z "${SPIRA_DB:-}" ]; then
        printf 'suite-state-fence: SPIRA_DB not set — cannot verify bead %s for %s\n' \
            "$bead" "$suite" >&2
        rc=1
        continue
    fi
    bead_status=""
    bead_status="$("$BDCMD" -C "$SPIRA_DB" show "$bead" --json 2>/dev/null \
        | python3 -c '
import json, sys
try:
    d = json.load(sys.stdin)
    d = d[0] if isinstance(d, list) else d
    print(d.get("status",""))
except Exception:
    pass
' 2>/dev/null)" || bead_status=""
    if [ -z "${bead_status:-}" ]; then
        printf 'suite-state-fence: cannot verify bead %s for %s — refusing to land unchecked\n' \
            "$bead" "$suite" >&2
        rc=1
    elif [ "$bead_status" = "closed" ]; then
        printf 'suite-state-fence: %s is quarantined against CLOSED bead %s; closed bead is nobody'\''s work\n' \
            "$suite" "$bead" >&2
        rc=1
    fi
done < <(suite_state_parse "$STATE_FILE" 2>/dev/null)

if [ "$rc" -eq 0 ]; then
    _n="$(suite_state_parse "$STATE_FILE" 2>/dev/null | wc -l | tr -d ' ')"
    printf 'suite-state-fence: clean — %s non-default entry/entries\n' "$_n"
fi
exit "$rc"
