#!/usr/bin/env bash
#
# test-incident-delivers-satisfiable.sh — the delivers: label incident.sh stamps is a
# criterion a session on THIS install can actually satisfy (UC-ops-detection-remediation-08).
#
#   ./test-incident-delivers-satisfiable.sh
#
# A delivers: label is a CLOSING CRITERION: the sentinel reopens a bead closed without a
# commit unless the named path was written during the session. incident.sh stamped
# delivers:note:$SPIRA_RUN/sop/applied.jsonl on EVERY bead it filed, and nothing ever
# created that directory — eleven open beads carried an unsatisfiable criterion. Two more
# carried an absolute path from a DIFFERENT install, having travelled between machines.
#
# THIS WAS A SOURCE-GREP SUITE (regex over incident.sh's own text; "mutually exclusive"
# passed if `else` appeared anywhere). Replaced here with a real T1 behaviour test: a stub
# bd captures every `--label` written, in a scratch SPIRA_RUN, so the suite proves what the
# code DOES rather than what it appears to say.
#
# MERGED FROM test-batch-repeat-refused-delivers.sh (deleted): its two cases mirrored this
# exact contract for testenv-batch.sh's repeat-refused filing shape, at T2 against a real
# fixture database it never needed — case 5 below reproduces that env shape against the stub.
#
# tier: T1
# covers: spira/incident.sh spira/incident-stub-bd.py spira/testenv-batch.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

echo "test-incident-delivers-satisfiable.sh"

INC="$HERE/incident.sh"
STUB_BD="$HERE/incident-stub-bd.py"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
mkdir -p "$TMP/home" "$TMP/run"
printf '#!/usr/bin/env bash\nexit 0\n' > "$TMP/home/mail.sh"; chmod +x "$TMP/home/mail.sh"

export STUB_BD_STATE="$TMP/state.json" STUB_BD_LOG="$TMP/bd.log"

file_one() {  # file_one <ref> [VAR=val ...]
    local ref="$1"; shift
    printf 'payload' | env -i HOME="$HOME" PATH="$PATH" \
        SPIRA_BD="$STUB_BD" STUB_BD_STATE="$STUB_BD_STATE" STUB_BD_LOG="$STUB_BD_LOG" \
        SPIRA_DB="fakedb" SPIRA_RUN="$TMP/run" SPIRA_CONF="$TMP/no-conf" \
        SPIRA_HOME="$TMP/home" \
        SPIRA_INCIDENT_REF="$ref" \
        SPIRA_INCIDENT_LOCK="$TMP/run/delivers-test.lock" \
        SPIRA_INCIDENT_REPO= \
        "$@" bash "$INC" file "delivers test" - >/dev/null 2>&1
}
bead_of() {
    python3 -c '
import json
d = json.load(open("'"$STUB_BD_STATE"'"))
for b in d["beads"].values():
    if b.get("external_ref") == "'"$1"'": print(b["id"]); break
'
}
delivers_labels() {
    python3 -c '
import json
d = json.load(open("'"$STUB_BD_STATE"'"))
print(" ".join(l for l in d["beads"].get("'"$1"'", {}).get("labels", []) if l.startswith("delivers:")))
'
}

# ======================================================================================
echo
echo "1. the default: no SPIRA_INCIDENT_DELIVERS set — delivers:action (evidence is the close reason):"
# ======================================================================================
rm -f "$STUB_BD_STATE" "$STUB_BD_LOG"
file_one "incident:d-default"
bid1="$(bead_of "incident:d-default")"
[ -n "$bid1" ] && ok "bead was created" || bad "bead was created" "none found"
want "delivers:action is stamped by default" "delivers:action" "$(delivers_labels "$bid1")"

# ======================================================================================
echo
echo "2. SPIRA_INCIDENT_DELIVERS=note, ledger inside SPIRA_RUN: delivers:note:<ledger> is reachable and stamped:"
# ======================================================================================
rm -f "$STUB_BD_STATE" "$STUB_BD_LOG"
LEDGER_IN="$TMP/run/sop/applied.jsonl"
file_one "incident:d-note-in" SPIRA_INCIDENT_DELIVERS=note SPIRA_SOP_LEDGER="$LEDGER_IN"
bid2="$(bead_of "incident:d-note-in")"
want "delivers:note:<ledger> is stamped, not delivers:action" "delivers:note:$LEDGER_IN" "$(delivers_labels "$bid2")"
is "the ledger's directory is created, so the criterion is reachable" "yes" \
    "$([ -d "$(dirname "$LEDGER_IN")" ] && echo yes || echo no)"

# ======================================================================================
echo
echo "3. SPIRA_INCIDENT_DELIVERS=note, ledger OUTSIDE SPIRA_RUN: falls back to delivers:action, and says why:"
# ======================================================================================
# A path outside this install's SPIRA_RUN can never be written by a session here — beads
# travel between machines carrying an absolute path from wherever they were filed.
rm -f "$STUB_BD_STATE" "$STUB_BD_LOG"
LEDGER_OUT="$TMP/elsewhere/applied.jsonl"
ILOG="$TMP/run/incident.log"
file_one "incident:d-note-out" SPIRA_INCIDENT_DELIVERS=note SPIRA_SOP_LEDGER="$LEDGER_OUT" SPIRA_INCIDENT_LOG="$ILOG"
bid3="$(bead_of "incident:d-note-out")"
_labels3="$(delivers_labels "$bid3")"
want   "an outside-SPIRA_RUN ledger falls back to delivers:action" "delivers:action" "$_labels3"
nowant "and delivers:note: is never stamped for it"                "delivers:note:"  "$_labels3"
want   "the fallback is logged, not silent"                        "outside" "$(cat "$ILOG" 2>/dev/null)"

# ======================================================================================
echo
echo "4. SPIRA_INCIDENT_DELIVERS=<unrecognised>: no label is written, and it is logged:"
# ======================================================================================
rm -f "$STUB_BD_STATE" "$STUB_BD_LOG"
ILOG4="$TMP/run/incident4.log"
file_one "incident:d-bogus" SPIRA_INCIDENT_DELIVERS=bogus SPIRA_INCIDENT_LOG="$ILOG4"
bid4="$(bead_of "incident:d-bogus")"
is "no delivers: label of any kind is written for an unrecognised value" "" "$(delivers_labels "$bid4")"
want "the rejection is logged, not silent" "unrecognised" "$(cat "$ILOG4" 2>/dev/null)"

# ======================================================================================
echo
echo "5. SPIRA_INCIDENT_DELIVERS=action (testenv-batch.sh's repeat-refused shape, merged from test-batch-repeat-refused-delivers.sh):"
# ======================================================================================
# Mirrors testenv-batch.sh's own call shape exactly, which files a repeat-refused
# diagnosis bead — a correct diagnosis has no ledger to write, so delivers:action (the
# close reason is the evidence) is the only criterion it can ever satisfy.
rm -f "$STUB_BD_STATE" "$STUB_BD_LOG"
file_one "repeat-refused:spira/test-br:0000000000000000" \
    SPIRA_INCIDENT_DELIVERS=action SPIRA_INCIDENT_TYPE=task SPIRA_INCIDENT_CAUSE=repeat-refused \
    SPIRA_INCIDENT_PRIORITY=3
bid5="$(bead_of "repeat-refused:spira/test-br:0000000000000000")"
_labels5="$(delivers_labels "$bid5")"
want   "delivers:action is present"    "delivers:action" "$_labels5"
nowant "delivers:note: is absent"      "delivers:note:"  "$_labels5"

echo
tl_summary
