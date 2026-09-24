#!/usr/bin/env bash
#
# test-batch-repeat-refused-delivers.sh — repeat-refused beads get delivers:action, not sop-ledger.
#
# PROPERTY. testenv-batch.sh files repeat-refused beads through incident.sh with
# SPIRA_INCIDENT_DELIVERS=action. The resulting bead must carry delivers:action and
# must NOT carry delivers:note: (the SOP-ledger label that the correct completion of
# a diagnosis bead cannot produce).
#
# POSITIVE CONTROL (law-absence-needs-a-positive-control, law-a-regression-test-must-be-seen-to-fail).
# Before asserting the fixed label, this suite files an identical bead WITHOUT
# SPIRA_INCIDENT_DELIVERS and requires delivers:note: to appear. That verifies the
# label-reader is sound: if it returns nothing at all, the negative assertion in the
# fixed case would pass vacuously.
#
# The filing path mirrors testenv-batch.sh lines 407-413 exactly — same env vars,
# same incident.sh call, throwaway fixture database (law-prefer-the-real-dependency).
#
# defect: sp-4aa1h
# covers: spira/testenv-batch.sh spira/incident.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "${2:-}"; }
want()   { case "$3" in *"$2"*) ok "$1" ;; *) bad "$1" "wanted [$2] in [$3]"; esac; }
nowant() { case "$3" in *"$2"*) bad "$1" "did not want [$2] in [$3]" ;; *) ok "$1"; esac; }

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-batch-repeat-refused-delivers
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
export SPIRA_TESTDB_MODE=server
testdb_up rr_delivers || {
    printf 'SKIP test-batch-repeat-refused-delivers: server testdb not available\n' >&2
    exit 77
}

INC="$HERE/incident.sh"
RUN="$TMP/run"; mkdir -p "$RUN"

# Stub mail.sh so no real escalation fires.
mkdir -p "$TMP/inc-home"
cat > "$TMP/inc-home/mail.sh" <<'M'
#!/usr/bin/env bash
exit 0
M
chmod +x "$TMP/inc-home/mail.sh"

# file_rr: file a repeat-refused bead exactly as testenv-batch.sh does (lines 407-413).
# Extra env vars can be appended as NAME=val arguments.
file_rr() {  # file_rr [NAME=val ...]
    printf 'Repeat attempt refused. Prior red at some-time. Key: batch-test-key. Branch: spira/test-br.\n\nFirst: bash spira/testenv-batch.sh --suites test-foo.sh spira/test-br\nIf tests now pass, the fix was committed after the retry was refused — close with evidence. If they still fail, investigate.\n' \
    | env SPIRA_DB="$SPIRA_DB" \
          SPIRA_RUN="$RUN" \
          SPIRA_CONF="$TMP/no-conf" \
          SPIRA_HOME="$TMP/inc-home" \
          SPIRA_INCIDENT_LOCK="$RUN/rr-test.lock" \
          SPIRA_INCIDENT_REF="repeat-refused:spira/test-br:0000000000000000" \
          SPIRA_INCIDENT_REPO= \
          SPIRA_INCIDENT_TYPE=task \
          SPIRA_INCIDENT_CAUSE=repeat-refused \
          SPIRA_INCIDENT_PRIORITY=3 \
          "$@" \
          bash "$INC" file "repeat attempt: no change — spira/test-br" - 2>/dev/null
}

# Read delivers labels for a bead.
delivers_labels() {  # delivers_labels <bead-id>
    bd -C "$SPIRA_DB" label list "$1" 2>/dev/null | grep 'delivers:' || true
}

echo "test-batch-repeat-refused-delivers.sh"

# ==============================================================================
echo
echo "POSITIVE CONTROL: with SPIRA_INCIDENT_DELIVERS=note, delivers:note: is stamped"
# ==============================================================================
# This proves the label-reader is sound. If it returned nothing here, the
# nowant assertion in the fixed case would pass even if no label was written at all.
# The default is now delivers:action; we force delivers:note: via SPIRA_INCIDENT_DELIVERS=note
# to get a non-action label the reader can detect.
testdb_reset; mkdir -p "$RUN"
file_rr SPIRA_INCIDENT_DELIVERS=note >/dev/null
pc_id="$(bd -C "$SPIRA_DB" list --status open --limit 0 --label spira,incident --json 2>/dev/null \
    | python3 -c '
import json,sys
try: d=json.load(sys.stdin)
except: sys.exit(0)
d=d if isinstance(d,list) else [d]
for i in d:
    if (i.get("external_ref") or "").startswith("repeat-refused:"): print(i["id"]); break
' 2>/dev/null)"
[ -n "$pc_id" ] || { bad "positive-control bead was created" "none found"; \
    printf '%s: %d passed, %d failed\n' "$(basename "$0")" "$pass" "$fail"; exit 1; }

pc_labels="$(delivers_labels "$pc_id")"
want "positive control: delivers:note: appears with SPIRA_INCIDENT_DELIVERS=note" "delivers:note:" "$pc_labels"

# ==============================================================================
echo
echo "FIXED CASE: with SPIRA_INCIDENT_DELIVERS=action (testenv-batch.sh path), delivers:action is stamped"
# ==============================================================================
testdb_reset; mkdir -p "$RUN"
file_rr SPIRA_INCIDENT_DELIVERS=action >/dev/null
fixed_id="$(bd -C "$SPIRA_DB" list --status open --limit 0 --label spira,incident --json 2>/dev/null \
    | python3 -c '
import json,sys
try: d=json.load(sys.stdin)
except: sys.exit(0)
d=d if isinstance(d,list) else [d]
for i in d:
    if (i.get("external_ref") or "").startswith("repeat-refused:"): print(i["id"]); break
' 2>/dev/null)"
[ -n "$fixed_id" ] || { bad "fixed-case bead was created" "none found"; \
    printf '%s: %d passed, %d failed\n' "$(basename "$0")" "$pass" "$fail"; exit 1; }

fixed_labels="$(delivers_labels "$fixed_id")"
want   "delivers:action is present"    "delivers:action" "$fixed_labels"
nowant "delivers:note: is absent"      "delivers:note:"  "$fixed_labels"

echo
printf '%s: %d passed, %d failed\n' "$(basename "$0")" "$pass" "$fail"
[ "$fail" -eq 0 ]
