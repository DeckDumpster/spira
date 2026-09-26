#!/usr/bin/env bash
#
# incident-cause-lint.sh — every SPIRA_INCIDENT_REF filing site also declares
# SPIRA_INCIDENT_CAUSE (law-producers-declare-what-they-know).
#
#   incident-cause-lint.sh              scan spira/*.sh (excluding test suites); exit 1 naming offenders
#   incident-cause-lint.sh --dir DIR    scan DIR instead of this script's own directory
#
# THE PROPERTY. A producer that sets SPIRA_INCIDENT_REF without SPIRA_INCIDENT_CAUSE files
# recurrences into the undifferentiated "unrecorded" bucket, collapsing the census taxonomy
# and preventing Maechen's ranking step from telling causes apart.
#
# THE ACCEPTANCE CRITERION. Every SPIRA_INCIDENT_REF= assignment in spira/*.sh (excluding
# test suites, which plant both real and deliberately-bare examples) must carry
# SPIRA_INCIDENT_CAUSE somewhere in its surrounding 14 lines (10 before, 3 after) — wide
# enough to span a multi-line env-var block without reaching into an unrelated call above.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"

# check_undeclared <dir> — emit one "UNDECLARED <file>:<line>" per SPIRA_INCIDENT_REF
# site that lacks a SPIRA_INCIDENT_CAUSE in its surrounding window.
check_undeclared() {
    local dir="$1"
    grep -rn "SPIRA_INCIDENT_REF=" --include='*.sh' "$dir" | grep -v '/test-' | \
    while IFS=: read -r f l r; do
        [ "$(sed -n "$((l-10)),$((l+3))p" "$f" | grep -c SPIRA_INCIDENT_CAUSE)" -eq 0 ] \
            && printf 'UNDECLARED %s:%s\n' "$f" "$l"
    done
}

DIR="$HERE"
case "${1:-}" in
--dir) DIR="${2:?--dir needs a directory}" ;;
esac

out="$(check_undeclared "$DIR")"
if [ -n "$out" ]; then
    printf 'incident-cause-lint: SPIRA_INCIDENT_REF sites without SPIRA_INCIDENT_CAUSE:\n' >&2
    printf '%s\n' "$out" >&2
    exit 1
fi
printf 'incident-cause-lint: clean\n'
exit 0
