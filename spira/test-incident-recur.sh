#!/usr/bin/env bash
#
# test-incident-recur.sh — a LANDED incident refiles fresh, citing its predecessor; recurrence notes stay bounded
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

echo "test-incident-recur.sh"

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-incident-recur
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up incident_recur || { echo "test-incident-recur: could not build a fixture database"; exit 1; }
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
echo "a recurrence of a LANDED incident files a fresh bead citing its predecessor (sp-nmlna):"
# ======================================================================================
# A LANDED row is terminal — the machine never leaves it — so reopening the bead in bd would
# only pile recurrence notes under a row that stays LANDED, every event inside the lookback.
# The recurrence is a new incident: a fresh bead (with its own row), its body citing the
# closed one, linked to it by a relates-to dep; the closed bead is left as it was.
# SEEN TO FAIL against the unfixed tree: count_all stayed 1 and the log said "reopened from
# closed" (the regression is run::tests::a_recurrence_of_a_landed_incident_files_a_fresh_
# bead_citing_its_predecessor; this is its end-to-end twin against a real bd).
printf 'landed first payload\n' | inc >/dev/null
_ld_id="$(timeout 5 bd -C "$SPIRA_DB" list --status open,in_progress --limit 0 --json 2>/dev/null \
  | python3 -c '
import sys, json
target = sys.argv[1]
try: d = json.load(sys.stdin)
except Exception: sys.exit(0)
for i in (d if isinstance(d, list) else [d]):
    if i.get("external_ref") == target:
        print(i["id"]); break
' 'incident:the-test-sweep')"
want "the first LANDED-case bead was filed" "sp-" "${_ld_id:-none found}"
testdb_restate "${_ld_id:-}" closed || true      # fixture data; the mirror's closed is LANDED
> "$ILOG"
printf 'landed second payload\n' | inc >/dev/null 2>&1
# The intake's log names the bead it filed: "filed <id> for <ref> — recurrence of <old> ...".
_ld_new="$(sed -n 's/.*incident: filed \([^ ]*\) for .*recurrence of.*/\1/p' "$ILOG" | tail -1)"
is "a LANDED incident's recurrence leaves two beads (the closed one and a fresh one)" "2" "$(count_all 'incident:the-test-sweep')"
is "exactly one of them is open" "1" "$(count_open 'incident:the-test-sweep')"
is "the fresh bead is a different bead" "yes" "$([ -n "$_ld_new" ] && [ "$_ld_new" != "${_ld_id:-}" ] && echo yes || echo no)"
is "the closed bead stays closed in bd (never reopened)" "closed" \
    "$(timeout 5 bd -C "$SPIRA_DB" show "${_ld_id:-none}" --json 2>/dev/null | python3 -c '
import sys, json
lines = sys.stdin.read().splitlines()
at = next((i for i, l in enumerate(lines) if l[:1] in "[{"), None)
d = json.loads("\n".join(lines[at:])) if at is not None else {}
d = d[0] if isinstance(d, list) and d else d
print(d.get("status", "") if isinstance(d, dict) else "")')"
is "no reopen was logged" "0" "$(grep -c 'reopened from closed' "$ILOG" 2>/dev/null || true)"
want "the fresh bead's body cites its predecessor" "Recurrence of ${_ld_id:-none} (predecessor)" \
    "$(timeout 5 bd -C "$SPIRA_DB" show "${_ld_new:-none}" --json 2>/dev/null)"
want "the fresh bead is related to its predecessor" "\"id\": \"${_ld_id:-none}\"" \
    "$(timeout 5 bd -C "$SPIRA_DB" dep list "${_ld_new:-none}" --type relates-to --json 2>/dev/null)"
want "the filing was logged as a recurrence of the closed bead" "recurrence of ${_ld_id:-none}" "$(cat "$ILOG")"

testdb_reset
mkdir -p "$RUN"
> "$ILOG"

testdb_reset; mkdir -p "$RUN"; > "$ILOG"

# ======================================================================================
echo
echo "recurrence notes are bounded for an unchanged payload (UC-06, one real row — the decision itself is T1 above):"
# ======================================================================================
# The pure note-size decision (_recur_note_body) is table-tested with no database in
# test-incident-decisions.sh; this is the one real row proving the wiring — that file_one
# actually calls it and that the resulting bead's notes field reflects the decision.
REF_BOUND="incident:test-recur-bounded"
PAYLOAD500="$(python3 -c 'print("x" * 500, end="")')"
for _i in 1 2 3; do
    printf '%s' "$PAYLOAD500" | inc SPIRA_INCIDENT_REF="$REF_BOUND" >/dev/null
done
_bound_id="$(timeout 5 bd -C "$SPIRA_DB" list --status open,in_progress --limit 0 --json 2>/dev/null \
  | python3 -c '
import sys, json
target = sys.argv[1]
try: d = json.load(sys.stdin)
except Exception: sys.exit(0)
for i in (d if isinstance(d, list) else [d]):
    if i.get("external_ref") == target: print(i["id"]); break
' "$REF_BOUND")"
want "recur-bounded bead was created" "sp-" "${_bound_id:-none found}"
# The first recurrence has no prior payload-hash label, so it writes the payload once; the
# second recurrence's payload is byte-identical, so it must NOT write it again — that is the
# whole property _recur_note_body exists to enforce. A copy count of exactly 1 (not 0, not 2)
# proves both halves at once: the payload was recorded at all, and it was not re-recorded.
_payload_copies="$(timeout 5 bd -C "$SPIRA_DB" show "${_bound_id:-?}" --json 2>/dev/null | python3 -c '
import sys, json
try:
    d = json.load(sys.stdin); d = d if isinstance(d, list) else [d]
    notes = (d[0].get("notes") or "") if d else ""
    print(notes.count("x" * 500))
except Exception: print(-1)
')"
is "an unchanged 500-byte payload is recorded exactly once across 3 identical filings" "1" "$_payload_copies"

tl_summary
