#!/usr/bin/env bash
#
# test-sin-exempt.sh — a SIN-exempt ref recurs without paging; a non-exempt ref still pages.
#
#   ./test-sin-exempt.sh
#
# THE TWO CONTROLS (law-absence-needs-a-positive-control):
#
#   1. A non-exempt ref that recurs past SIN_AT gets the `sin` label and triggers the
#      escalation path. Without this, the exemption is indistinguishable from a SIN that
#      never fires at all.
#   2. An exempt ref that recurs past SIN_AT does NOT get the `sin` label and does NOT
#      trigger escalation. Its recurrence counter still advances and its notes still record
#      what happened — only the page is suppressed.
#
# WHY THIS SUITE EXISTS. The watchtower files a sweep on every ten-minute pass. incident.sh
# dedupes on the external ref and bumps sp-recur-N. N counts intervals in which nobody closed
# a routine health report, not unremediated failures. At SIN_AT=5 the page fires after fifty
# minutes of quiet — zero defects. Closing the bead re-arms the cycle. $18/day of aeon cost
# (measured sp-kufh). SPIRA_SIN_EXEMPT=1 is the brake: the watchtower sets it so its ref
# cannot reach the SIN escalation.
#
# A REAL bd ON A FIXTURE DATABASE (law-prefer-the-real-dependency), because the test is about
# what labels and notes incident.sh writes through bd.
#
# defect: sp-kufh
# covers: spira/incident.sh spira/watchtower.sh
# hermetic-ok: uses a fixture database, no systemd or gh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-sin-exempt
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up sinex || { echo "test-sin-exempt: could not build a fixture database"; exit 1; }
# lib.sh provides recurs_of for event-based recurrence counting (sp-recur-N labels
# are no longer written; sp-lzt moved the count to the events table).
# shellcheck disable=SC1090
. "$HERE/lib.sh"

INC="$HERE/incident.sh"
B() { bd -C "$SPIRA_DB" "$@"; }
mkdir -p "$TMP/run"

# The ask stub records calls rather than reaching a real escalation path.
cat > "$TMP/ask.sh" <<'A'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$ASK_LOG"
A
chmod +x "$TMP/ask.sh"
export ASK_LOG="$TMP/ask.log"

labels_of() { B show "$1" --json 2>/dev/null | python3 -c '
import json, sys
d = json.load(sys.stdin); d = d if isinstance(d, list) else [d]
print(" ".join(d[0].get("labels") or []))'; }
has_label() { [[ " $(labels_of "$1") " == *" $2 "* ]]; }
# recur_max: how many recurrences the store has recorded against a bead.
#
# THIS USED TO READ sp-recur-N LABELS, AND THEY ARE NOT WRITTEN ANY MORE (sp-lzt). incident.sh
# records a recurrence as an `recurred` EVENT and says so in its own comment; the labels went
# away with it. The label read kept working — it found nothing, returned 0, and every
# assertion here compared 0 against the expected count, so five FAILs pointed at the sin
# escalation while the thing they were reading had simply moved. test-suites.sh was updated
# for the same change; this suite was missed.
#
# recurs_of is lib.sh's own reader for that counter and handles all three storage paths
# (bd sql in server mode, the dolt CLI in embedded mode, and the events.log file fallback).
# Using it rather than a private query is also what keeps this suite honest when the storage
# moves again.
# shellcheck disable=SC1090
. "$HERE/lib.sh" 2>/dev/null || true
recur_max() {
    local _n; _n="$(recurs_of "$1" 2>/dev/null)"
    case "$_n" in ''|*[!0-9]*) echo 0 ;; *) echo "$_n" ;; esac
}

# file_incident <ref> <title> <payload> [VAR=val ...]
# Runs incident.sh file once with the given ref and title on stdin.
file_incident() {
    local ref="$1" title="$2" payload="$3"; shift 3
    printf '%s' "$payload" | \
        env SPIRA_DB="$SPIRA_DB" SPIRA_RUN="$TMP/run" SPIRA_CONF="$TMP/no-conf" \
        SPIRA_INCIDENT_REF="$ref" SPIRA_ASK="$TMP/ask.sh" \
        SPIRA_INCIDENT_LOCK="$TMP/run/sinex-test.lock" \
        SPIRA_INCIDENT_REPO= \
        "$@" \
        bash "$INC" file "$title" - 2>/dev/null
}

echo "test-sin-exempt.sh"

# ======================================================================================
echo
echo "the positive control — a non-exempt ref reaches SIN:"
# ======================================================================================
# File the same ref SIN_AT + 1 times. SIN_AT counts RECURRENCES, not sightings — incident.sh
# escalates "past SIN_AT recurrences" and its counter starts at zero on the filing that
# CREATES the bead. So reaching a count of SIN_AT takes one create plus SIN_AT recurring
# filings. Filing SIN_AT times left the counter at SIN_AT - 1, one short of the threshold, so
# the sin label was correctly withheld and this positive control asserted against a state the
# loop never produced.
testdb_reset
: > "$ASK_LOG"
SIN_AT=3
ref="incident:test-nonexempt-sin"
title="non-exempt incident"
for i in $(seq 1 "$(( SIN_AT + 1 ))"); do
    file_incident "$ref" "$title" "payload $i" SPIRA_SIN_AT="$SIN_AT" >/dev/null
done

# Find the bead by its external ref.  --external-ref is dev-build only; filter in Python.
bid="$(B list --status open --limit 0 --label spira,incident --json 2>/dev/null \
    | python3 -c '
import json,sys
target=sys.argv[1]
try: d=json.load(sys.stdin)
except: sys.exit(0)
d=d if isinstance(d,list) else [d]
for i in d:
    if i.get("external_ref")==target: print(i["id"]); break
' "$ref" 2>/dev/null)"
[ -n "$bid" ] && ok "the bead was created" || bad "the bead was created" "no bead found for ref $ref"

if [ -n "$bid" ]; then
    has_label "$bid" sin && ok "the non-exempt bead gets the sin label" \
        || bad "the non-exempt bead gets the sin label" "labels: $(labels_of "$bid")"
    is "the recurrence counter reached $SIN_AT" "$SIN_AT" "$(recur_max "$bid")"
    want "the ask was filed" "recurred" "$(cat "$ASK_LOG" 2>/dev/null)"
fi

# ======================================================================================
echo
echo "an exempt ref does NOT reach SIN:"
# ======================================================================================
# Same SIN_AT+1 total filings as the positive control, to reach recurs_of == SIN_AT.
testdb_reset
: > "$ASK_LOG"
ref="incident:test-exempt-sin"
title="exempt incident"
for i in $(seq 1 "$(( SIN_AT + 1 ))"); do
    file_incident "$ref" "$title" "payload $i" SPIRA_SIN_AT="$SIN_AT" SPIRA_SIN_EXEMPT=1 >/dev/null
done

bid="$(B list --status open --limit 0 --label spira,incident --json 2>/dev/null \
    | python3 -c '
import json,sys
target=sys.argv[1]
try: d=json.load(sys.stdin)
except: sys.exit(0)
d=d if isinstance(d,list) else [d]
for i in d:
    if i.get("external_ref")==target: print(i["id"]); break
' "$ref" 2>/dev/null)"
[ -n "$bid" ] && ok "the exempt bead was created" || bad "the exempt bead was created" "no bead found for ref $ref"

if [ -n "$bid" ]; then
    has_label "$bid" sin && bad "the exempt bead does NOT get the sin label" "labels: $(labels_of "$bid")" \
        || ok "the exempt bead does NOT get the sin label"
    is "the recurrence counter still advances" "$SIN_AT" "$(recur_max "$bid")"
    is "the ask was NOT filed" "" "$(cat "$ASK_LOG" 2>/dev/null | tr -d '[:space:]')"
fi

# ======================================================================================
echo
echo "an exempt ref past the threshold still increments:"
# ======================================================================================
# File one more beyond SIN_AT. The counter should still advance.
file_incident "$ref" "$title" "payload extra" SPIRA_SIN_AT="$SIN_AT" SPIRA_SIN_EXEMPT=1 >/dev/null
if [ -n "$bid" ]; then
    is "the counter advances past SIN_AT" "$(( SIN_AT + 1 ))" "$(recur_max "$bid")"
    has_label "$bid" sin \
        && bad "still no sin label after passing SIN_AT" "labels: $(labels_of "$bid")" \
        || ok "still no sin label after passing SIN_AT"
fi

# ======================================================================================
echo
echo "the watchtower sets SPIRA_SIN_EXEMPT=1:"
# ======================================================================================
# Structural: the routine snapshot | pipe was removed (sp-8wshp); verify that the
# DRAINING escalation — the first incident.sh call that remains — still carries the
# exemption, so drain beads cannot reach the SIN escalation either.
want "watchtower.sh sets SPIRA_SIN_EXEMPT=1 on the DRAINING escalation" "SPIRA_SIN_EXEMPT=1" \
    "$(grep -B5 'file "DRAINING:' "$HERE/watchtower.sh" 2>/dev/null)"

echo
printf '%s: %d passed, %d failed\n' "$(basename "$0")" "$pass" "$fail"
[ "$fail" -eq 0 ]
