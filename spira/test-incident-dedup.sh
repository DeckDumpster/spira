#!/usr/bin/env bash
#
# test-incident-dedup.sh — incident.sh _dedup_incident() catches all duplicates with matching external_ref
#
#   ./test-incident-dedup.sh
#
# THE DEFECTS THIS SUITE GUARDS AGAINST.
#
# sp-csvzn: bd list --json omitted the external_ref field. _dedup_incident reads external_ref
# from bd list --json output; when the field was absent bead.get('external_ref') returned None
# for every candidate, so every filing looked like "no open incident" and filed a fresh bead.
# 17-18+ surplus beads per external_ref were observed in production.
#
# sp-ew54u: Python variable expansion bug — $SPIRA_BD or $SPIRA_DB inside a single-quoted
# Python heredoc block was not expanded by the shell, so the Python code received the literal
# string "$SPIRA_BD" rather than the database path. The bdq call then addressed the caller's
# default store instead of the configured one, silently finding no candidates.
#
# sp-vq796: _dedup_incident did not check the external_ref field when iterating candidates.
# The label-keyed path (ref:<hash>) could return at most 1 candidate, but if the check
# against external_ref was absent, ANY bead with a matching label (even one from a hash
# collision) would be treated as the incumbent — or, if the Python code did not reach the
# comparison at all, every lookup returned empty and every filing went to bdq create.
#
# WHY THREE DISTINCT DEFECTS NEED ONE UNIFIED SUITE. All three surface identically from the
# outside: more than one bead exists for a given external_ref after N filings. A single suite
# that files N times and asserts count=1 is the minimal property that catches any of them, and
# it is exactly the test that did not exist when they shipped.
#
# POSITIVE CONTROL IS FIRST (law-absence-needs-a-positive-control). Before asserting that N
# filings dedupe to 1, the suite proves that N direct insertions produce N beads. If the count
# helper is broken or SPIRA_DB is mis-set, the positive control fails, and the dedup test is
# not trusted. A suite that skips this step cannot distinguish "dedupe works" from "count is
# always 0".
#
# The suite also explicitly verifies that bd list --json includes external_ref (sp-csvzn).
# A missing field is not caught by asserting count=1 if the dedup happens to work for other
# reasons; the field assertion is the positive-control check for sp-csvzn specifically.
#
# Driven through the REAL incident.sh against a REAL bd on a throwaway fixture database.
# The dedup is a database read followed by a conditional write; a stub cannot reproduce the
# failure modes above — each requires a real schema (law-prefer-the-real-dependency).
#
# defect: sp-ls8kw (test suite for sp-csvzn, sp-ew54u, sp-vq796)
# covers: spira/incident.sh
# timeout: 300
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()  { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want(){ [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }

echo "test-incident-dedup.sh"

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-incident-dedup
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up incident-dedup || { echo "test-incident-dedup: could not build a fixture database"; exit 1; }

NOOP="$TMP/noop.sh"
printf '#!/usr/bin/env bash\nexit 0\n' > "$NOOP"; chmod +x "$NOOP"

RUN="$TMP/run"; mkdir -p "$RUN"
SPOOL="$RUN/spool"
LOCK="$RUN/incident.lock"
ILOG="$RUN/incident.log"

# Run incident.sh with an explicit, named environment pointing at the fixture database.
# HOME is the real one because bd and dolt read their credentials from it.
# SPIRA_CONF names a non-existent file so a real spira.conf on this box cannot override keys
# the suite sets explicitly (law-gates-run-in-a-clean-environment).
inc() {
    env -i HOME="$HOME" PATH="$PATH" SPIRA_PATH="${SPIRA_PATH:-}" \
        SPIRA_CONF="$TMP/nonexistent.conf" \
        SPIRA_DB="$SPIRA_DB" \
        SPIRA_SPOOL="$SPOOL" \
        SPIRA_INCIDENT_LOG="$ILOG" \
        SPIRA_INCIDENT_LOCK="$LOCK" \
        SPIRA_RUN="$RUN" \
        SPIRA_NOTIFY="$NOOP" \
        "$@" bash "$HERE/incident.sh" file "dedup test incident" -
}

# Count open beads carrying the given external_ref on the fixture database.
# Filters client-side on external_ref — required because bd-embedded does not support
# --external-ref server-side filtering (same approach as incident.sh _dedup_incident).
count_by_ref() {    # count_by_ref <external-ref> -> integer
    bd -C "$SPIRA_DB" list --status open,in_progress --limit 0 --label spira,incident --json 2>/dev/null \
      | python3 -c '
import sys, json
target = sys.argv[1]; count = 0
try: d = json.load(sys.stdin)
except Exception: print(0); raise SystemExit(0)
for i in (d if isinstance(d, list) else [d]):
    if i.get("external_ref") == target:
        count += 1
print(count)
' "$1"
}

# The external_ref that incident.sh derives from the title "dedup test incident".
DEDUP_REF="incident:dedup-test-incident"
N=3   # number of duplicate filings; 3 > 2 exercises recurrence past the sequential-pair case

# ======================================================================================
echo
echo "external_ref in bd list --json — field must be present in the response (sp-csvzn):"
# ======================================================================================
# WHAT sp-csvzn BROKE. bd list --json omitted external_ref, so bead.get('external_ref')
# returned None for every candidate. _dedup_incident always printed nothing, and every
# filing called bdq create instead of bumping the existing bead's recurrence counter.
#
# POSITIVE CONTROL (law-absence-needs-a-positive-control). Create one bead with a known
# external_ref, then confirm that field appears in bd list --json. If it does not appear,
# the dedup assertions below test nothing meaningful — the field check is the gating proof.
bd -C "$SPIRA_DB" create "external-ref field probe" \
    --type bug --priority 2 --labels spira,incident \
    --external-ref "probe:external-ref-field-check" --silent >/dev/null 2>&1 || true

_raw_json="$(bd -C "$SPIRA_DB" list --status open --limit 0 --json 2>/dev/null)"
_has_field="$(printf '%s\n' "$_raw_json" | python3 -c '
import sys, json
try:
    d = json.load(sys.stdin)
    rows = d if isinstance(d, list) else [d]
    target = "probe:external-ref-field-check"
    for r in rows:
        if r.get("external_ref") == target:
            print("yes"); raise SystemExit(0)
    print("no")
except Exception:
    print("no")
' 2>/dev/null)"
is "bd list --json includes external_ref field with the correct value" "yes" "$_has_field"

testdb_reset; mkdir -p "$RUN"; > "$ILOG"

# ======================================================================================
echo
echo "positive control — N direct inserts create N beads (proves the counter works):"
# ======================================================================================
# THE SHAPE OF THE BUG (sp-vq796). Without dedup, filing N incidents for the same
# external_ref produces N beads. This control plants N beads directly — bypassing incident.sh —
# and confirms count_by_ref returns N. If count_by_ref returns anything other than N here,
# the counting mechanism is broken and the dedup assertions below cannot be trusted.
#
# A check that finds count=1 unconditionally (e.g. because SPIRA_DB is mis-pointed and no
# beads exist) is indistinguishable from "dedup works" without this control.
BD_REAL="${SPIRA_BD:-bd}"
for _i in $(seq 1 $N); do
    "$BD_REAL" -C "$SPIRA_DB" create "direct-insert-$_i" \
        --type bug --priority 2 --labels spira,incident \
        --external-ref "$DEDUP_REF" --silent >/dev/null 2>&1
done
n_direct="$(count_by_ref "$DEDUP_REF")"
is "positive control: $N direct inserts produce $N distinct beads" "$N" "$n_direct"

testdb_reset; mkdir -p "$RUN"; > "$ILOG"

# ======================================================================================
echo
echo "N-filing dedup — filing the same incident $N times produces exactly one bead:"
# ======================================================================================
# THE PROPERTY NONE OF sp-csvzn, sp-ew54u, sp-vq796 SATISFIED. Each defect caused
# _dedup_incident to return empty on every call, so file_one always reached bdq create.
# N filings produced N beads. This assertion catches any of the three defects returning:
# if _dedup_incident cannot find an existing bead with the right external_ref, count stays
# above 1 after the second filing.
for _i in $(seq 1 $N); do
    printf 'filing %d\n' "$_i" | inc >/dev/null
done
n_dedup="$(count_by_ref "$DEDUP_REF")"
is "$N filings of the same external_ref produce exactly one bead" "1" "$n_dedup"

# VERIFY RECURRENCES ARE LOGGED. sp-recur-N-<cause> labels are no longer written (sp-lzt);
# recurrences are recorded as events and logged in the incident log. The first of N filings
# creates the bead (no recurrence entry) and each of the remaining N-1 filings logs a
# recurrence — so the incident log must contain exactly N-1 "recurred" lines.
_recur_count="$(grep -c ' recurred ' "$ILOG" 2>/dev/null || true)"
is "$((N-1)) recurrences logged after $N filings" "$((N-1))" "$_recur_count"

testdb_reset; mkdir -p "$RUN"; > "$ILOG"

# ======================================================================================
echo
echo "dedup with SPIRA_INCIDENT_REF set — explicit ref collapses N filings to one bead:"
# ======================================================================================
# sp-ew54u INVOLVED PYTHON CODE THAT PASSED $SPIRA_DB AS A LITERAL. The current code
# passes all variables as sys.argv positional arguments to the Python subprocess —
# never inside single-quoted Python string literals where the shell cannot expand them.
# This sub-test exercises the exact path: SPIRA_INCIDENT_REF sets an explicit external ref
# and the Python comparison must resolve correctly against $SPIRA_DB's beads.
#
# A Python variable-expansion bug would cause the bdq call to address the wrong database,
# so _dedup_incident would always return empty and each of the N filings would create a new
# bead. count_by_ref returning N (not 1) is the failure signature.
EXPLICIT_REF="incident:explicit-ref-dedup-test"
for _i in $(seq 1 $N); do
    printf 'explicit ref filing %d\n' "$_i" | inc SPIRA_INCIDENT_REF="$EXPLICIT_REF" >/dev/null
done
n_explicit="$(bd -C "$SPIRA_DB" list --status open,in_progress --limit 0 --label spira,incident --json 2>/dev/null \
  | python3 -c '
import sys, json
target = sys.argv[1]; count = 0
try: d = json.load(sys.stdin)
except Exception: print(0); raise SystemExit(0)
for i in (d if isinstance(d, list) else [d]):
    if i.get("external_ref") == target: count += 1
print(count)
' "$EXPLICIT_REF")"
is "$N filings with SPIRA_INCIDENT_REF produce exactly one bead (sp-ew54u)" "1" "$n_explicit"

testdb_reset; mkdir -p "$RUN"; > "$ILOG"

# ======================================================================================
echo
echo "surplus absence — no beads exist beyond the single incumbent after $N filings:"
# ======================================================================================
# THE OBSERVED FAILURE MODE. sp-vq796 produced 17-18 surplus beads per external_ref.
# This case queries the TOTAL open bead count after N filings and asserts it is 1.
# It catches both the duplicate-filing defect AND any case where the dedup creates extra
# beads that are not keyed to the target ref (e.g. a labelling or create bug that
# produces beads with a slightly different external_ref but the same title).
for _i in $(seq 1 $N); do
    printf 'surplus check filing %d\n' "$_i" | inc >/dev/null
done
n_total="$(bd -C "$SPIRA_DB" list --status open,in_progress --limit 0 --label spira,incident --json 2>/dev/null \
  | python3 -c '
import sys, json
try:
    d = json.load(sys.stdin)
    print(len(d) if isinstance(d, list) else (1 if isinstance(d, dict) and d else 0))
except: print(0)
')"
is "total open incident count is 1 after $N filings — no surplus beads (sp-vq796)" "1" "$n_total"

testdb_reset; mkdir -p "$RUN"; > "$ILOG"

# ======================================================================================
echo
echo "cross-label dedup — two filers with different SPIRA_INCIDENT_LABELS dedupe to one bead:"
# ======================================================================================
# THE DEFECT THIS GUARDS. _dedup_incident built its prefilter as an AND over the
# non-repo labels from LABELS (_dedupe_labels) plus ref:<hash>. Two filers of the same
# event that declare different SPIRA_INCIDENT_LABELS therefore queried non-overlapping
# partitions and never found each other's open bead. Result: one event, two beads, two
# Ops sessions working identical snapshots.
#
# THE INVARIANT. The prefilter must key on ref:<hash> ALONE. ref:<hash> is derived from
# external_ref and cannot vary between filers of one event. No label the caller chooses
# may appear in that filter.
#
# REGRESSION SHAPE. Filing with LABELS=spira,plan creates a bead labelled plan,spira.
# Filing again with LABELS=spira,incident builds _dedupe_labels=spira,incident; if that
# AND filter is applied, the query cannot return the plan-labelled bead and a second bead
# is created. The fix (ref:<hash> only) finds the first bead regardless of its labels.
CROSS_REF="incident:cross-label-dedup-test"

# count_by_ref_any queries ALL open/in_progress beads without a label filter, then
# filters client-side on external_ref. Required here because the two filers leave beads
# with different labels, so count_by_ref (which filters on spira,incident) would miss the
# first bead.
count_by_ref_any() {
    bd -C "$SPIRA_DB" list --status open,in_progress --limit 0 --json 2>/dev/null \
      | python3 -c '
import sys, json
target = sys.argv[1]; count = 0
try: d = json.load(sys.stdin)
except Exception: print(0); raise SystemExit(0)
for i in (d if isinstance(d, list) else [d]):
    if i.get("external_ref") == target:
        count += 1
print(count)
' "$1"
}

# Filer 1 uses the builder partition (plan); filer 2 uses the ops partition (incident).
# Both declare the same SPIRA_INCIDENT_REF so external_ref is identical.
printf 'first filer\n' | inc SPIRA_INCIDENT_LABELS="spira,plan" SPIRA_INCIDENT_REF="$CROSS_REF" >/dev/null
printf 'second filer\n' | inc SPIRA_INCIDENT_LABELS="spira,incident" SPIRA_INCIDENT_REF="$CROSS_REF" >/dev/null

n_cross="$(count_by_ref_any "$CROSS_REF")"
is "cross-label: two filers with different LABELS produce exactly one bead" "1" "$n_cross"

# RECURRENCE MUST HAVE BEEN LOGGED. Recurrence is recorded via ilog "... recurred ..."
# in the incident log rather than via sp-recur-N labels (which are no longer written).
# If filer 2 found the existing bead it records "recurred"; if it created a fresh bead
# the log contains two "filed" entries and no "recurred" — the log check distinguishes
# dedupe-and-bump from two separate filings that happened to produce one bead.
_cross_recur_logged="$(grep -c ' recurred ' "$ILOG" 2>/dev/null || true)"
is "cross-label: second filer records recurrence in incident log" "1" "$_cross_recur_logged"

echo
printf '%s: %d passed, %d failed\n' "$(basename "$0")" "$pass" "$fail"
[ "$fail" -eq 0 ]
