#!/usr/bin/env bash
#
# test-incident-cause-rule.sh — every SPIRA_INCIDENT_REF filing site declares SPIRA_INCIDENT_CAUSE.
#
# tier: T0
# covers: spira-lint/src/rules/incident_cause_lint.rs spira/*.sh UC-ops-detection-remediation-09
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
ROOT="$(cd "$HERE/.." && pwd -P)"
. "$HERE/testlib.sh"

echo "test-incident-cause-rule.sh"
command -v spira-lint >/dev/null 2>&1 || bail "spira-lint is not on PATH"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
git init -q "$TMP/t" && mkdir -p "$TMP/t/spira"
lint() { spira-lint --root "$1" --only incident-cause-lint 2>&1; }

printf '#!/usr/bin/env bash\nSPIRA_INCIDENT_REF=incident:planted bash incident.sh\n' >"$TMP/t/spira/offender.sh"
git -C "$TMP/t" add -A
out="$(lint "$TMP/t")"; rc=$?
is   "a planted ref with no cause is refused" 1 "$rc"
want "the finding names the offender line" "spira/offender.sh:2" "$out"

printf 'SPIRA_INCIDENT_CAUSE=planted\n' >>"$TMP/t/spira/offender.sh"
git -C "$TMP/t" add -A
out="$(lint "$TMP/t")"; rc=$?
is "a declared cause beside the ref is clean" 0 "$rc"

out="$(lint "$ROOT")"; rc=$?
is "the tree declares a cause at every ref site" 0 "$rc"
[ "$rc" = 0 ] || printf '%s\n' "$out"

tl_summary
