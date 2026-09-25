#!/usr/bin/env bash
#
# test-incident-systemd.sh — `incident.sh systemd <unit>` (gap G2: the OnFailure= intake path).
#
#   ./test-incident-systemd.sh
#
# THE GAP. `mtgc-alert-<store>@.service` fires from OnFailure= on every failed unit and
# calls `incident.sh systemd <unit>`. install-intake.sh wires it; test-incident.sh's own
# comments name it as one of three paths through drain_one, but nothing had ever invoked
# it — the path a real production failure actually takes was untested end to end.
#
# WHAT THIS SUITE HOLDS. The unit becomes both the external ref (incident:<unit>, so a
# flapping unit dedupes to one bead) and the bead's title, and the intake behaves the same
# way through this call site as through `incident.sh file`: an unreachable database leaves
# the filing spooled rather than dropping it. systemctl/journalctl are not stubbed —
# incident.sh's own `|| echo '(... unavailable)'` fallback already covers their absence,
# and this suite has no systemd user session to make that path interesting to reproduce.
#
# tier: T2
# covers: spira/incident.sh spira/incident-stub-bd.py spira/install-intake.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

echo "test-incident-systemd.sh"

INC="$HERE/incident.sh"
STUB_BD="$HERE/incident-stub-bd.py"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
mkdir -p "$TMP/home" "$TMP/run"
printf '#!/usr/bin/env bash\nexit 0\n' > "$TMP/home/mail.sh"; chmod +x "$TMP/home/mail.sh"
export STUB_BD_STATE="$TMP/state.json" STUB_BD_LOG="$TMP/bd.log"

sysinc() {  # sysinc <unit>
    env -i HOME="$HOME" PATH="$PATH" \
        SPIRA_BD="$STUB_BD" STUB_BD_STATE="$STUB_BD_STATE" STUB_BD_LOG="$STUB_BD_LOG" \
        SPIRA_DB="fakedb" SPIRA_RUN="$TMP/run" SPIRA_CONF="$TMP/no-conf" SPIRA_HOME="$TMP/home" \
        SPIRA_INCIDENT_LOCK="$TMP/run/systemd-test.lock" \
        SPIRA_INCIDENT_CAUSE=systemd-fail \
        bash "$INC" systemd "$1"
}
bead_of() {
    python3 -c '
import json
d = json.load(open("'"$STUB_BD_STATE"'"))
for b in d["beads"].values():
    if b.get("external_ref") == "incident:'"$1"'": print(b["id"]); break
'
}

# ======================================================================================
echo
echo "a failed unit files exactly one bead, keyed on incident:<unit>:"
# ======================================================================================
rm -f "$STUB_BD_STATE" "$STUB_BD_LOG"
out="$(sysinc "spira-test-unit.service" 2>&1)"; rc=$?
is "systemd <unit> exits 0 when the database is reachable" "0" "$rc"
bid="$(bead_of "spira-test-unit.service")"
[ -n "$bid" ] && ok "a bead was filed for incident:<unit>" \
    || bad "a bead was filed for incident:<unit>" "none found; output: $out"

# ======================================================================================
echo
echo "the gathered payload names the unit even with no systemctl/journalctl on PATH:"
# ======================================================================================
_notes="$(python3 -c '
import json
d = json.load(open("'"$STUB_BD_STATE"'"))
print(d["beads"].get("'"${bid:-?}"'", {}).get("notes", ""))
')"
_bead_title="$(python3 -c '
import json
d = json.load(open("'"$STUB_BD_STATE"'"))
print(d["beads"].get("'"${bid:-?}"'", {}).get("title", ""))
')"
want "the bead title names the failed unit" "spira-test-unit.service" "$_bead_title"

# ======================================================================================
echo
echo "a second failure of the same unit dedupes to a recurrence, not a second bead:"
# ======================================================================================
sysinc "spira-test-unit.service" >/dev/null 2>&1
_second_bid="$(bead_of "spira-test-unit.service")"
is "the second filing finds the same bead (dedup on incident:<unit>)" "$bid" "$_second_bid"
_recur_events="$(python3 -c '
import json
d = json.load(open("'"$STUB_BD_STATE"'"))
print(sum(1 for e in d["events"] if e["issue_id"] == "'"$bid"'" and e["event_type"] == "recurred"))
')"
is "the second failure is recorded as one recurrence" "1" "$_recur_events"

# ======================================================================================
echo
echo "an unreachable database leaves the systemd-triggered filing spooled, not lost:"
# ======================================================================================
rm -rf "$TMP/run/incident-spool"; mkdir -p "$TMP/run/incident-spool"
out2="$(env -i HOME="$HOME" PATH="$PATH" \
    SPIRA_BD="$TMP/no-such-bd" \
    SPIRA_DB="fakedb" SPIRA_RUN="$TMP/run" SPIRA_CONF="$TMP/no-conf" SPIRA_HOME="$TMP/home" \
    SPIRA_INCIDENT_LOCK="$TMP/run/systemd-test.lock" \
    bash "$INC" systemd "spira-db-down-unit.service" 2>&1)"; rc2=$?
is "exits non-zero when the database is unreachable" "1" "$rc2"
_spooled="$(find "$TMP/run/incident-spool" -maxdepth 1 -type f ! -name '*.bad' 2>/dev/null | wc -l | tr -d ' ')"
is "the OnFailure= filing stays spooled rather than being dropped" "1" "${_spooled:-0}"

echo
tl_summary
