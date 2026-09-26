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
# DEMOTED TO T1 (UC-ops-detection-remediation-07). recurs_of -> _counter_events_query needs
# only row 3 of `bd sql`'s output, which embedded testdb refuses entirely (it answers every
# `bd sql` with "not yet supported") and server-mode testdb costs ~110s to prove it. Neither
# is needed: incident-stub-bd.py is a genuinely stateful fake bd (a second filing must see
# the first filing's write, and a bump_recur event must be counted by the next recurs_of
# call), so the whole sequence — create, recur, recur, escalate — runs against it exactly as
# it would against real bd, without a fixture database at all.
#
# The DRAINING-escalation exemption claim ("watchtower.sh sets SPIRA_SIN_EXEMPT=1") is
# SOURCE-GREPPED from test-watchtower.sh instead of here, which already captures the
# incident env on that call site.
#
# defect: sp-kufh
# tier: T1
# covers: spira/incident.sh spira/incident-stub-bd.py
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

echo "test-sin-exempt.sh"

INC="$HERE/incident.sh"
STUB_BD="$HERE/incident-stub-bd.py"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
mkdir -p "$TMP/home" "$TMP/run"

export STUB_BD_STATE="$TMP/state.json" STUB_BD_LOG="$TMP/bd.log" MAIL_LOG="$TMP/mail.log"
cat > "$TMP/home/mail.sh" <<'M'
#!/usr/bin/env bash
[ "${1:-}" = send ] || exit 0
printf '%s\n' "$*" >> "$MAIL_LOG"
cat >> "$MAIL_LOG"
M
chmod +x "$TMP/home/mail.sh"

file_incident() {  # file_incident <ref> <title> <payload> [VAR=val ...]
    local ref="$1" title="$2" payload="$3"; shift 3
    printf '%s' "$payload" | \
        env -i HOME="$HOME" PATH="$PATH" \
        SPIRA_BD="$STUB_BD" STUB_BD_STATE="$STUB_BD_STATE" STUB_BD_LOG="$STUB_BD_LOG" \
        MAIL_LOG="$MAIL_LOG" \
        SPIRA_DB="fakedb" SPIRA_RUN="$TMP/run" SPIRA_CONF="$TMP/no-conf" \
        SPIRA_HOME="$TMP/home" \
        SPIRA_INCIDENT_REF="$ref" \
        SPIRA_INCIDENT_LOCK="$TMP/run/sinex-test.lock" \
        SPIRA_INCIDENT_REPO= \
        "$@" \
        bash "$INC" file "$title" - 2>/dev/null
}

bead_of() {  # bead_of <ref> -> id
    python3 -c '
import json
d = json.load(open("'"$STUB_BD_STATE"'"))
for b in d["beads"].values():
    if b.get("external_ref") == "'"$1"'": print(b["id"]); break
'
}
labels_of() {
    python3 -c '
import json
d = json.load(open("'"$STUB_BD_STATE"'"))
print(" ".join(d["beads"].get("'"$1"'", {}).get("labels", [])))
'
}
has_label() { [[ " $(labels_of "$1") " == *" $2 "* ]]; }
recur_max() {
    python3 -c '
import json
d = json.load(open("'"$STUB_BD_STATE"'"))
print(sum(1 for e in d["events"] if e["issue_id"] == "'"$1"'" and e["event_type"] == "recurred"))
'
}

# ======================================================================================
echo
echo "the positive control — a non-exempt ref reaches SIN:"
# ======================================================================================
# File the same ref SIN_AT + 1 times. SIN_AT counts RECURRENCES, not sightings — incident.sh
# escalates "past SIN_AT recurrences" and its counter starts at zero on the filing that
# CREATES the bead. So reaching a count of SIN_AT takes one create plus SIN_AT recurring
# filings.
SIN_AT=3
ref="incident:test-nonexempt-sin"
title="non-exempt incident"
for i in $(seq 1 "$(( SIN_AT + 1 ))"); do
    file_incident "$ref" "$title" "payload $i" SPIRA_SIN_AT="$SIN_AT" >/dev/null
done

bid="$(bead_of "$ref")"
[ -n "$bid" ] && ok "the bead was created" || bad "the bead was created" "no bead found for ref $ref"

if [ -n "$bid" ]; then
    has_label "$bid" sin && ok "the non-exempt bead gets the sin label" \
        || bad "the non-exempt bead gets the sin label" "labels: $(labels_of "$bid")"
    is "the recurrence counter reached $SIN_AT" "$SIN_AT" "$(recur_max "$bid")"
    want "the ask was filed" "recurred" "$(cat "$MAIL_LOG" 2>/dev/null)"
fi

# ======================================================================================
echo
echo "an exempt ref does NOT reach SIN:"
# ======================================================================================
rm -f "$STUB_BD_STATE" "$STUB_BD_LOG"; : > "$MAIL_LOG"
ref="incident:test-exempt-sin"
title="exempt incident"
for i in $(seq 1 "$(( SIN_AT + 1 ))"); do
    file_incident "$ref" "$title" "payload $i" SPIRA_SIN_AT="$SIN_AT" SPIRA_SIN_EXEMPT=1 >/dev/null
done

bid="$(bead_of "$ref")"
[ -n "$bid" ] && ok "the exempt bead was created" || bad "the exempt bead was created" "no bead found for ref $ref"

if [ -n "$bid" ]; then
    has_label "$bid" sin && bad "the exempt bead does NOT get the sin label" "labels: $(labels_of "$bid")" \
        || ok "the exempt bead does NOT get the sin label"
    is "the recurrence counter still advances" "$SIN_AT" "$(recur_max "$bid")"
    is "the ask was NOT filed" "" "$(cat "$MAIL_LOG" 2>/dev/null | tr -d '[:space:]')"
fi

# ======================================================================================
echo
echo "an exempt ref past the threshold still increments:"
# ======================================================================================
file_incident "$ref" "$title" "payload extra" SPIRA_SIN_AT="$SIN_AT" SPIRA_SIN_EXEMPT=1 >/dev/null
if [ -n "$bid" ]; then
    is "the counter advances past SIN_AT" "$(( SIN_AT + 1 ))" "$(recur_max "$bid")"
    has_label "$bid" sin \
        && bad "still no sin label after passing SIN_AT" "labels: $(labels_of "$bid")" \
        || ok "still no sin label after passing SIN_AT"
fi

tl_summary
