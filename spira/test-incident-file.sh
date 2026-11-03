#!/usr/bin/env bash
#
# test-incident-file.sh — a first filing creates one bead, and an explicit SPIRA_INCIDENT_REF dedups against $SPIRA_DB
#
# Split from the one test-incident.sh for the 30 s cap: every `incident file` is a few seconds of
# real bd round trips, so the scenarios run as test-incident.sh, test-incident-file.sh,
# test-incident-dedup.sh and test-incident-recur.sh, each on a fixture of its own. Driven through
# the REAL incident.sh against a REAL bd (law-prefer-the-real-dependency).
#
# defect: sp-5ll6
# tier: T2
# covers: spira/incident.sh watchtower/src/* incident/* UC-ops-detection-remediation-01 UC-ops-detection-remediation-02 UC-ops-detection-remediation-03
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"
tl_config SPIRA_TEST_CASE_DEADLINE=60

echo "test-incident-file.sh"

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-incident-file
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up incident_file || { echo "test-incident-file: could not build a fixture database"; exit 1; }
BD_REAL="${SPIRA_BD:-bd}"
# sp-jgjvh: incident beads are work beads, so the dedup reads each one's state from its
# lifecycle row, never bd status. With no lifecycle store here, a stand-in spira-lc tells
# this database's bd story in lifecycle terms (testlib.sh lc_mirror_bd: open → READY,
# closed → LANDED); a machine that does not answer would leave every filing spooled.
lc_mirror_bd "$TMP/lc"


RUN="$TMP/run"; mkdir -p "$RUN"
SPOOL="$RUN/spool"
LOCK="$RUN/incident.lock"
ILOG="$RUN/incident.log"

# Run incident.sh with an explicit, named environment, pointing at the fixture database and
# isolated scratch directories (law-gates-run-in-a-clean-environment). HOME is the real one
# because bd and dolt read their credentials from it; SPIRA_PATH is passed because conf.sh
# rebuilds PATH from it; SPIRA_CONF names a non-existent file so a real spira.conf on this
# box cannot override any key the suite sets explicitly.
inc() {
    tl_config SPIRA_PATH="${SPIRA_PATH:-}" SPIRA_DB="$SPIRA_DB" SPIRA_RUN="$RUN" \
        SPIRA_MAIL="$TMP/mail"
    local extra=() _a
    for _a in "$@"; do
        case "$_a" in
            SPIRA_BD=*) tl_config "$_a" ;;
            *) extra+=("$_a") ;;
        esac
    done
    env -i SPIRA_RELEASE="${SPIRA_RELEASE:-}" HOME="$HOME" PATH="$PATH" \
        SPIRA_TOML="$SPIRA_TOML" \
        SPIRA_CONF="$TMP/nonexistent.conf" \
        SPIRA_SPOOL="$SPOOL" \
        SPIRA_INCIDENT_LOG="$ILOG" \
        SPIRA_INCIDENT_LOCK="$LOCK" \
        SPIRA_HOME="$HERE" \
        SPIRA_LC_BIN="$SPIRA_LC_BIN" \
        "${extra[@]+"${extra[@]}"}" incident.sh file "the test sweep" -
}

# Count open beads carrying the given external ref on the fixture database.
# NOTE: bd-embedded does not support --external-ref server-side filtering, so filter
# client-side via JSON, exactly as incident.sh open_incident does.
count_open() {
    timeout 5 bd -C "$SPIRA_DB" list --status open,in_progress --limit 0 --label spira,incident --json 2>/dev/null \
      | python3 -c '
import sys, json
target = sys.argv[1]
count = 0
try: d = json.load(sys.stdin)
except Exception: sys.exit(0)
for i in (d if isinstance(d, list) else [d]):
    if i.get("external_ref") == target:
        count += 1
print(count)
' "$1"
}

# Count ALL beads (any status) carrying the given external ref on the fixture database.
# Used by the close-then-refile regression: after the fix the closed bead is reopened so
# the total count stays 1; under the unfixed code a new bead is created and the count is 2.
count_all() {
    timeout 5 bd -C "$SPIRA_DB" list --all --status open,in_progress,closed --limit 0 --label spira,incident --json 2>/dev/null \
      | python3 -c '
import sys, json
target = sys.argv[1]
count = 0
try: d = json.load(sys.stdin)
except Exception: sys.exit(0)
for i in (d if isinstance(d, list) else [d]):
    if i.get("external_ref") == target:
        count += 1
print(count)
' "$1"
}

# ======================================================================================
echo
echo "the positive control — a single call creates one bead and open_incident finds it:"
# ======================================================================================
# If this fails, open_incident's query (--external-ref, --label spira,incident) is not
# finding what incident.sh filed, and every concurrent-case assertion below is testing the
# wrong thing.
printf 'positive control payload\n' | inc >/dev/null
n="$(count_open 'incident:the-test-sweep')"
is "a single filing creates exactly one bead" "1" "$n"

testdb_reset
mkdir -p "$RUN"

# ======================================================================================
echo
echo "dedup with SPIRA_INCIDENT_REF set — the explicit-ref path resolves against \$SPIRA_DB, not a literal (sp-ew54u):"
# ======================================================================================
# THE DEFECT (sp-ew54u, merged from test-incident-dedup.sh). \$SPIRA_DB inside a
# single-quoted Python heredoc was never shell-expanded, so the query addressed the
# caller's default store instead of the configured one and every filing found nothing.
# The current code passes it as a sys.argv positional, never inside a Python string
# literal; this exercises exactly that path.
EXPLICIT_REF="incident:explicit-ref-dedup-test"
for _i in 1 2 3; do
    printf 'explicit ref filing %d\n' "$_i" | inc SPIRA_INCIDENT_REF="$EXPLICIT_REF" >/dev/null
done
n_explicit="$(count_open "$EXPLICIT_REF")"
is "3 filings with SPIRA_INCIDENT_REF produce exactly one bead (sp-ew54u)" "1" "$n_explicit"

tl_summary
