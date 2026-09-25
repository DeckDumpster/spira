#!/usr/bin/env bash
#
# test-incident-spool-drain.sh — incident.sh's write-ahead spool survives a database that
# cannot be reached (gap G1, UC-ops-detection-remediation-10).
#
#   ./test-incident-spool-drain.sh
#
# WHY THIS SUITE EXISTS. incident.sh writes the payload to disk BEFORE touching the
# database specifically so a production event is not lost when Dolt is down or `bd` times
# out — but nothing exercised that path. This is the path that runs when the DB is down,
# which is exactly when an incident matters most, and gap G1 named it as untested.
#
# FAILING-FIRST (law-a-regression-test-must-be-seen-to-fail). Case 1 is run first against
# a genuinely unreachable bd (a path to nothing) so the suite proves the write-ahead
# behaviour is real, not an artifact of a stub that happens to always succeed.
#
# tier: T2
# covers: spira/incident.sh spira/incident-stub-bd.py
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

echo "test-incident-spool-drain.sh"

INC="$HERE/incident.sh"
STUB_BD="$HERE/incident-stub-bd.py"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
mkdir -p "$TMP/home" "$TMP/run"
printf '#!/usr/bin/env bash\nexit 0\n' > "$TMP/home/mail.sh"; chmod +x "$TMP/home/mail.sh"
SPOOL="$TMP/run/incident-spool"
ILOG="$TMP/run/incident.log"

# spool_entry <ref-or-empty> <title> <cause-or-empty> <body> -> writes a spool file by hand,
# in incident.sh's own spool_write format, so entries missing a field (an older-format
# entry, or a REF-less probe) can be constructed directly.
spool_entry() {
    local ref="$1" title="$2" cause="$3" body="$4" path
    path="$SPOOL/$(date -u +%Y%m%dT%H%M%SZ)-manual-$$-$RANDOM"
    {
        [ -n "$ref" ] && printf 'REF: %s\n' "$ref"
        printf 'TITLE: %s\n' "$title"
        printf 'UNIT: ?\nINCIDENT_PATH: ?\n'
        [ -n "$cause" ] && printf 'CAUSE: %s\n' "$cause"
        printf -- '--\n%s' "$body"
    } > "$path"
    printf '%s' "$path"
}
spool_count() { find "$SPOOL" -maxdepth 1 -type f ! -name '*.bad' 2>/dev/null | wc -l | tr -d ' '; }
bad_count()   { find "$SPOOL" -maxdepth 1 -type f -name '*.bad' 2>/dev/null | wc -l | tr -d ' '; }

# ======================================================================================
echo
echo "1. SEEN TO FAIL FIRST: an unreachable database leaves the filing spooled, not lost:"
# ======================================================================================
BROKEN_BD="$TMP/no-such-bd"
rm -rf "$SPOOL"; mkdir -p "$SPOOL"; : > "$ILOG"
out="$(printf 'db is down' | env -i HOME="$HOME" PATH="$PATH" \
    SPIRA_BD="$BROKEN_BD" \
    SPIRA_DB="fakedb" SPIRA_RUN="$TMP/run" SPIRA_CONF="$TMP/no-conf" SPIRA_HOME="$TMP/home" \
    SPIRA_INCIDENT_REF="incident:db-down-probe" \
    SPIRA_INCIDENT_LOCK="$TMP/run/spool-test.lock" \
    bash "$INC" file "db down probe" - 2>&1)"; rc=$?
is   "incident.sh file exits non-zero when the database is unreachable" "1" "$rc"
is   "the entry stays spooled, not deleted" "1" "$(spool_count)"
want "the caller is told it is spooled" "spooled" "$out"
want "the log names the cause: database unreachable" "database unreachable" "$(cat "$ILOG" 2>/dev/null)"

# ======================================================================================
echo
echo "2. incident.sh drain reports both counts, always — 'drained N, still spooled M':"
# ======================================================================================
rm -rf "$SPOOL"; mkdir -p "$SPOOL"; : > "$ILOG"
export STUB_BD_STATE="$TMP/state.json" STUB_BD_LOG="$TMP/bd.log"
rm -f "$STUB_BD_STATE" "$STUB_BD_LOG"
spool_entry "incident:drain-good-1" "good one" suite-red "payload 1" >/dev/null
spool_entry "incident:drain-good-2" "good two" suite-red "payload 2" >/dev/null

drain_out="$(env -i HOME="$HOME" PATH="$PATH" \
    SPIRA_BD="$STUB_BD" STUB_BD_STATE="$STUB_BD_STATE" STUB_BD_LOG="$STUB_BD_LOG" \
    SPIRA_DB="fakedb" SPIRA_RUN="$TMP/run" SPIRA_CONF="$TMP/no-conf" SPIRA_HOME="$TMP/home" \
    SPIRA_INCIDENT_LOCK="$TMP/run/spool-test.lock" \
    bash "$INC" drain 2>&1)"; rc=$?
want "drain reports 2 drained" "drained 2" "$drain_out"
want "and 0 still spooled"     "still spooled 0" "$drain_out"
is   "drain exits 0 when nothing is left stuck" "0" "$rc"
is   "both entries are removed from the spool" "0" "$(spool_count)"

# Now with an unreachable database, both must fail and stay spooled — the count on the
# other side of "drained N, still spooled M" is the untested half.
rm -rf "$SPOOL"; mkdir -p "$SPOOL"
spool_entry "incident:drain-stuck-1" "stuck one" suite-red "payload 1" >/dev/null
spool_entry "incident:drain-stuck-2" "stuck two" suite-red "payload 2" >/dev/null
drain_stuck_out="$(env -i HOME="$HOME" PATH="$PATH" \
    SPIRA_BD="$BROKEN_BD" \
    SPIRA_DB="fakedb" SPIRA_RUN="$TMP/run" SPIRA_CONF="$TMP/no-conf" SPIRA_HOME="$TMP/home" \
    SPIRA_INCIDENT_LOCK="$TMP/run/spool-test.lock" \
    bash "$INC" drain 2>&1)"; rc=$?
want "drain reports 0 drained when the database is down" "drained 0" "$drain_stuck_out"
want "and both entries still spooled" "still spooled 2" "$drain_stuck_out"
is   "drain exits non-zero while entries remain stuck" "1" "$rc"
is   "the stuck entries are left in the spool, not lost" "2" "$(spool_count)"

# ======================================================================================
echo
echo "3. a REF-less entry is quarantined to .bad, not retried forever:"
# ======================================================================================
rm -rf "$SPOOL"; mkdir -p "$SPOOL"; : > "$ILOG"
_bad_path="$(spool_entry "" "no ref here" suite-red "payload")"
env -i HOME="$HOME" PATH="$PATH" \
    SPIRA_BD="$STUB_BD" STUB_BD_STATE="$STUB_BD_STATE" STUB_BD_LOG="$STUB_BD_LOG" \
    SPIRA_DB="fakedb" SPIRA_RUN="$TMP/run" SPIRA_CONF="$TMP/no-conf" SPIRA_HOME="$TMP/home" \
    SPIRA_INCIDENT_LOCK="$TMP/run/spool-test.lock" \
    bash "$INC" drain >/dev/null 2>&1
is "the REF-less entry is moved to .bad" "1" "$(bad_count)"
is "the spool holds no live (non-.bad) entries after quarantine" "0" "$(spool_count)"
want "the log names why" "no REF" "$(cat "$ILOG" 2>/dev/null)"

# ======================================================================================
echo
echo "4. pre-CAUSE entries (no CAUSE: line — an older-format spool entry) default to unrecorded:"
# ======================================================================================
rm -rf "$SPOOL"; mkdir -p "$SPOOL"; rm -f "$STUB_BD_STATE" "$STUB_BD_LOG"
PRECAUSE_REF="incident:pre-cause-entry"
spool_entry "$PRECAUSE_REF" "pre-cause probe" "" "first filing" >/dev/null
env -i HOME="$HOME" PATH="$PATH" \
    SPIRA_BD="$STUB_BD" STUB_BD_STATE="$STUB_BD_STATE" STUB_BD_LOG="$STUB_BD_LOG" \
    SPIRA_DB="fakedb" SPIRA_RUN="$TMP/run" SPIRA_CONF="$TMP/no-conf" SPIRA_HOME="$TMP/home" \
    SPIRA_INCIDENT_LOCK="$TMP/run/spool-test.lock" \
    bash "$INC" drain >/dev/null 2>&1
# The FIRST filing only creates the bead (no recurrence event yet); a second, equally
# pre-CAUSE entry for the same ref is what exercises bump_recur, whose cause is what
# a pre-CAUSE entry must default to.
spool_entry "$PRECAUSE_REF" "pre-cause probe" "" "second filing" >/dev/null
env -i HOME="$HOME" PATH="$PATH" \
    SPIRA_BD="$STUB_BD" STUB_BD_STATE="$STUB_BD_STATE" STUB_BD_LOG="$STUB_BD_LOG" \
    SPIRA_DB="fakedb" SPIRA_RUN="$TMP/run" SPIRA_CONF="$TMP/no-conf" SPIRA_HOME="$TMP/home" \
    SPIRA_INCIDENT_LOCK="$TMP/run/spool-test.lock" \
    bash "$INC" drain >/dev/null 2>&1

_precause_cause="$(python3 -c '
import json
d = json.load(open("'"$STUB_BD_STATE"'"))
for b in d["beads"].values():
    if b.get("external_ref") == "'"$PRECAUSE_REF"'":
        for e in d["events"]:
            if e["issue_id"] == b["id"] and e["event_type"] == "recurred":
                print(e["new_value"])
        break
')"
is "a spool entry with no CAUSE: line records its recurrence as unrecorded" "unrecorded" "$_precause_cause"

echo
tl_summary
