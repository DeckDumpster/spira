#!/usr/bin/env bash
#
# test-fenrir-exempt.sh — a FENRIR-exempt ref recurs without paging; a non-exempt ref still pages.
#
#   ./test-fenrir-exempt.sh
#
# THE TWO CONTROLS (law-absence-needs-a-positive-control):
#
#   1. A non-exempt ref that recurs past FENRIR_AT gets the `fenrir` label and triggers the
#      escalation path. Without this, the exemption is indistinguishable from a FENRIR that
#      never fires at all.
#   2. An exempt ref that recurs past FENRIR_AT does NOT get the `fenrir` label and does NOT
#      trigger escalation. Its recurrence counter still advances and its notes still record
#      what happened — only the page is suppressed.
#
# WHY THIS SUITE EXISTS. The watchtower files a sweep on every ten-minute pass. incident.sh
# dedupes on the external ref and bumps sp-recur-N. N counts intervals in which nobody closed
# a routine health report, not unremediated failures. At FENRIR_AT=5 the page fires after fifty
# minutes of quiet — zero defects. Closing the bead re-arms the cycle. $18/day of dwarf cost
# (measured sp-kufh). SPIRA_FENRIR_EXEMPT=1 is the brake: the watchtower sets it so its ref
# cannot reach the FENRIR escalation.
#
# DEMOTED TO T1 (UC-ops-detection-remediation-07). recurs_of -> _counter_events_query needs
# only row 3 of `bd sql`'s output, which embedded testdb refuses entirely (it answers every
# `bd sql` with "not yet supported") and server-mode testdb costs ~110s to prove it. Neither
# is needed: incident-stub-bd is a genuinely stateful fake bd (a second filing must see
# the first filing's write, and a bump_recur event must be counted by the next recurs_of
# call), so the whole sequence — create, recur, recur, escalate — runs against it exactly as
# it would against real bd, without a fixture database at all.
#
# The DRAINING-escalation exemption claim ("watchtower sets SPIRA_FENRIR_EXEMPT=1") is
# SOURCE-GREPPED from test-watchtower.sh instead of here, which already captures the
# incident env on that call site.
#
# defect: sp-kufh
# tier: T1
# covers: spira/incident.sh incident/src/stub_bd.rs incident/* UC-ops-detection-remediation-07
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

echo "test-fenrir-exempt.sh"

STUB_BD="$(command -v incident-stub-bd)"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
mkdir -p "$TMP/home" "$TMP/run"
# round 2 fix: SPIRA_HOME IS the home now (locate_home no longer searches) and every
# binary reads <home>/conf.d to resolve its config schema at all — give this stub home
# the real registry.
ln -s "$HERE/conf.d" "$TMP/home/conf.d"
ln -s "$HERE/conf.toml.d" "$TMP/home/conf.toml.d"

export STUB_BD_STATE="$TMP/state.json" STUB_BD_LOG="$TMP/bd.log" MAIL_LOG="$TMP/mail.log"
# sp-jgjvh: incident beads are work beads, so incident's dedup reads each one's state from
# its lifecycle row, never bd status. A stand-in spira-lc tells the stub store's story in
# lifecycle terms (testlib.sh lc_mirror_bd: open → READY, closed → LANDED); with no machine
# every filing would stay spooled.
lc_mirror_bd "$TMP/lc"
cat > "$TMP/home/mail" <<'M'
#!/usr/bin/env bash
[ "${1:-}" = send ] || exit 0
printf '%s\n' "$*" >> "$MAIL_LOG"
cat >> "$MAIL_LOG"
M
chmod +x "$TMP/home/mail"

file_incident() {  # file_incident <ref> <title> <payload> [VAR=val ...]
    local ref="$1" title="$2" payload="$3"; shift 3
    # SPIRA_BD/SPIRA_DB/SPIRA_RUN are registered keys (per Ryan 2026-10-05, ONE SOURCE OF
    # CONFIG): declare via tl_config and thread SPIRA_TOML through env -i, which clears it.
    tl_config SPIRA_BD="$STUB_BD" SPIRA_DB="fakedb" SPIRA_RUN="$TMP/run"
    printf '%s' "$payload" | \
        env -i HOME="$HOME" PATH="$PATH" \
        STUB_BD_STATE="$STUB_BD_STATE" STUB_BD_LOG="$STUB_BD_LOG" SPIRA_LC_BIN="$SPIRA_LC_BIN" \
        MAIL_LOG="$MAIL_LOG" \
        SPIRA_CONF="$TMP/no-conf" \
        SPIRA_HOME="$TMP/home" PATH="$TMP/home:$PATH" \
        SPIRA_INCIDENT_REF="$ref" \
        SPIRA_INCIDENT_LOCK="$TMP/run/sinex-test.lock" \
        SPIRA_INCIDENT_REPO= \
        SPIRA_TOML="$SPIRA_TOML" \
        "$@" \
        incident.sh file "$title" - 2>/dev/null
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
recur_max() { lc_fact_count "$TMP/lc" "$1" recurred; }

# ======================================================================================
echo
echo "the positive control — a non-exempt ref reaches FENRIR:"
# ======================================================================================
# File the same ref FENRIR_AT + 1 times. FENRIR_AT counts RECURRENCES, not sightings — incident.sh
# escalates "past FENRIR_AT recurrences" and its counter starts at zero on the filing that
# CREATES the bead. So reaching a count of FENRIR_AT takes one create plus FENRIR_AT recurring
# filings.
FENRIR_AT=3
ref="incident:test-nonexempt-fenrir"
title="non-exempt incident"
for i in $(seq 1 "$(( FENRIR_AT + 1 ))"); do
    file_incident "$ref" "$title" "payload $i" SPIRA_FENRIR_AT="$FENRIR_AT" >/dev/null
done

bid="$(bead_of "$ref")"
[ -n "$bid" ] && ok "the bead was created" || bad "the bead was created" "no bead found for ref $ref"

if [ -n "$bid" ]; then
    has_label "$bid" fenrir && ok "the non-exempt bead gets the fenrir label" \
        || bad "the non-exempt bead gets the fenrir label" "labels: $(labels_of "$bid")"
    is "the recurrence counter reached $FENRIR_AT" "$FENRIR_AT" "$(recur_max "$bid")"
    want "the ask was filed" "recurred" "$(cat "$MAIL_LOG" 2>/dev/null)"
fi

# ======================================================================================
echo
echo "an exempt ref does NOT reach FENRIR:"
# ======================================================================================
rm -f "$STUB_BD_STATE" "$STUB_BD_LOG" "$TMP/lc/facts.tsv"; : > "$MAIL_LOG"
ref="incident:test-exempt-fenrir"
title="exempt incident"
for i in $(seq 1 "$(( FENRIR_AT + 1 ))"); do
    file_incident "$ref" "$title" "payload $i" SPIRA_FENRIR_AT="$FENRIR_AT" SPIRA_FENRIR_EXEMPT=1 >/dev/null
done

bid="$(bead_of "$ref")"
[ -n "$bid" ] && ok "the exempt bead was created" || bad "the exempt bead was created" "no bead found for ref $ref"

if [ -n "$bid" ]; then
    has_label "$bid" fenrir && bad "the exempt bead does NOT get the fenrir label" "labels: $(labels_of "$bid")" \
        || ok "the exempt bead does NOT get the fenrir label"
    is "the recurrence counter still advances" "$FENRIR_AT" "$(recur_max "$bid")"
    is "the ask was NOT filed" "" "$(cat "$MAIL_LOG" 2>/dev/null | tr -d '[:space:]')"
fi

# ======================================================================================
echo
echo "an exempt ref past the threshold still increments:"
# ======================================================================================
file_incident "$ref" "$title" "payload extra" SPIRA_FENRIR_AT="$FENRIR_AT" SPIRA_FENRIR_EXEMPT=1 >/dev/null
if [ -n "$bid" ]; then
    is "the counter advances past FENRIR_AT" "$(( FENRIR_AT + 1 ))" "$(recur_max "$bid")"
    has_label "$bid" fenrir \
        && bad "still no fenrir label after passing FENRIR_AT" "labels: $(labels_of "$bid")" \
        || ok "still no fenrir label after passing FENRIR_AT"
fi

tl_summary
