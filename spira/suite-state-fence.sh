#!/usr/bin/env bash
#
# suite-state-fence.sh — refuse a landing when spira/suite-state is malformed,
# names a non-existent suite, or keeps a quarantine alive on a CLOSED bead.
#
# The parse/lint logic moved to Rust (sp-9gd4e, DESIGN-suites.md §2.3/§6b):
# `testenv suites lint` is suite-state.sh's `suite_state_lint` + `suite_state_parse`,
# merged. This file keeps only what was never suite-state.sh's — locating that binary
# and the bd CLOSED-bead check below — and calls it by bare name on the launcher's PATH
# (sp-gypjk's convention; never a constructed path to it).
#
# A closed bead is nobody's work. suites.sh hygiene requires land_state:LANDED to
# reactivate a quarantine; CLOSED is never LANDED, so the check is permanently
# off with nobody accountable for it.
#
# Exits 0 when clean, 1 when any check fails.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"

STATE_FILE="${HERE%/}/../${SPIRA_SUITE_STATE_FILE:-spira/suite-state}"
if [ ! -r "$STATE_FILE" ]; then
    printf 'suite-state-fence: no state file — nothing to lint\n'
    exit 0
fi

if ! command -v testenv >/dev/null 2>&1; then
    printf 'suite-state-fence: testenv is not on PATH (the launcher sets PATH to a release) — refusing to lint unchecked\n' >&2
    exit 1
fi

LINT_ROWS="$(mktemp)"
trap 'rm -f "$LINT_ROWS"' EXIT

rc=0
testenv suites lint > "$LINT_ROWS" || rc=1

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
done < "$LINT_ROWS"

if [ "$rc" -eq 0 ]; then
    _n="$(wc -l < "$LINT_ROWS" | tr -d ' ')"
    printf 'suite-state-fence: clean — %s non-default entry/entries\n' "$_n"
fi
exit "$rc"
