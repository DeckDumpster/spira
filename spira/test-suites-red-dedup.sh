#!/usr/bin/env bash
#
# test-suites-red-dedup.sh — one open bead per red suite, however the failure changes
#
#   ./test-suites-red-dedup.sh
#
# THE DEFECT THIS SUITE GUARDS AGAINST. suites.sh filed its per-suite red beads under the
# external ref "suite:<name>:<fingerprint>", where the fingerprint is a hash of the suite's
# FAIL lines. That makes the dedupe key the SYMPTOM, not the suite: a suite that fails one
# way on one cycle and a different way on the next gets a second bead while the first is
# still open, and a third on the cycle after that. One timed pass produced 122 beads in a
# day, several of them the same suite filed repeatedly — test-beads-push-commit.sh,
# test-aeon-resume.sh and test-sop.sh each held two open beads for one broken suite.
#
# The key is now "suite:<name>" alone, so every failure of one suite lands on one bead and
# the differing fingerprints become recurrences on it. That is the property asserted here.
#
# WHY THE FINGERPRINT STILL EXISTS. It is recorded in the bead body, because knowing that
# the symptom CHANGED between cycles is worth having; it simply must not open a new bead.
#
# POSITIVE CONTROL IS FIRST (law-absence-needs-a-positive-control). Before asserting that
# two filings collapse to one bead, the suite proves that two filings for DIFFERENT suites
# produce two beads. Without it, a broken counter, a mis-set SPIRA_DB or an incident.sh that
# files nothing at all would report the dedupe as working while nothing was written.
#
# Driven through the REAL incident.sh against a REAL bd on a throwaway fixture database:
# the dedupe is a database read followed by a conditional write, and a stub reproduces only
# the surface the author remembered (law-prefer-the-real-dependency).
#
# covers: spira/suites.sh spira/incident.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()  { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }

echo "test-suites-red-dedup.sh"

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-suites-red-dedup
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up suites-red-dedup || {
    echo "test-suites-red-dedup: could not build a fixture database"; exit 1; }

NOOP="$TMP/noop.sh"; printf '#!/usr/bin/env bash\nexit 0\n' > "$NOOP"; chmod +x "$NOOP"
RUN="$TMP/run"; mkdir -p "$RUN"

# File one red exactly as suites.sh's file_red does: same type, actor, labels and cause,
# with the ref supplied by the caller so this suite can vary it.
# SPIRA_CONF names a file that does not exist so a real spira.conf cannot decide the verdict
# (law-gates-run-in-a-clean-environment).
file_red_with_ref() {   # file_red_with_ref <external-ref> <title>
    env -i HOME="$HOME" PATH="$PATH" SPIRA_PATH="${SPIRA_PATH:-}" \
        SPIRA_CONF="$TMP/nonexistent.conf" \
        SPIRA_DB="$SPIRA_DB" \
        SPIRA_SPOOL="$RUN/spool" \
        SPIRA_INCIDENT_LOG="$RUN/incident.log" \
        SPIRA_INCIDENT_LOCK="$RUN/incident.lock" \
        SPIRA_RUN="$RUN" \
        SPIRA_NOTIFY="$NOOP" \
        SPIRA_INCIDENT_TYPE=bug \
        SPIRA_INCIDENT_PRIORITY=2 \
        SPIRA_INCIDENT_ACTOR=suites \
        SPIRA_INCIDENT_LABELS=plan \
        SPIRA_INCIDENT_REF="$1" \
        SPIRA_INCIDENT_CAUSE=suite-red \
        SPIRA_SIN_EXEMPT=1 \
        bash "$HERE/incident.sh" file "$2" - <<<"body" >/dev/null 2>&1
}

# Count open beads whose external_ref is exactly the given value. Filtered client-side
# because bd-embedded has no server-side --external-ref filter.
count_by_ref() {    # count_by_ref <external-ref> -> integer
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

# ======================================================================================
echo
echo "POSITIVE CONTROL — two DIFFERENT suites must produce two beads:"
# ======================================================================================
# If this fails, every count below is meaningless: filing wrote nothing, or the counter
# cannot see what was written.
file_red_with_ref "suite:test-alpha.sh" "test-alpha.sh is red in the timed suite run"
file_red_with_ref "suite:test-beta.sh"  "test-beta.sh is red in the timed suite run"
is "test-alpha.sh has a bead"  1 "$(count_by_ref 'suite:test-alpha.sh')"
is "test-beta.sh has a bead"   1 "$(count_by_ref 'suite:test-beta.sh')"

# ======================================================================================
echo
echo "one suite failing three DIFFERENT ways holds exactly one open bead:"
# ======================================================================================
# The three filings stand for three timed cycles in which the same suite failed with
# different FAIL lines — which, under the old "suite:<name>:<fingerprint>" key, is exactly
# the shape that produced three beads.
for _ in 1 2 3; do
    file_red_with_ref "suite:test-gamma.sh" "test-gamma.sh is red in the timed suite run"
done
is "three filings, one bead" 1 "$(count_by_ref 'suite:test-gamma.sh')"

# ======================================================================================
echo
echo "CONTROL FOR THE KEY SHAPE — the OLD fingerprint-bearing key fragments:"
# ======================================================================================
# The assertion above holds for any single ref, so on its own it cannot tell a good key from
# a bad one: it would pass just as well under the old scheme, because each cycle simply used
# a different ref. Filing the same suite under the three refs the OLD scheme would have built
# shows the fragmentation directly — three open beads for one broken suite. This is what
# makes the previous assertion mean something.
for _fp in 1a2b3c 4d5e6f 7a8b9c; do
    file_red_with_ref "suite:test-delta.sh:$_fp" "test-delta.sh is red in the timed suite run"
done
_delta_total=0
for _fp in 1a2b3c 4d5e6f 7a8b9c; do
    _delta_total=$(( _delta_total + $(count_by_ref "suite:test-delta.sh:$_fp") ))
done
is "old key shape yields three beads for one suite" 3 "$_delta_total"

# ======================================================================================
echo
echo "the ref suites.sh emits carries no fingerprint:"
# ======================================================================================
# The behavioural assertions above hold for whatever ref THIS suite passes; they cannot see
# what suites.sh chooses. This checks the producer directly, so narrowing the key cannot be
# undone in suites.sh while the tests above keep passing.
_ref_line="$(grep -n 'SPIRA_INCIDENT_REF="suite:' "$HERE/suites.sh" | head -1)"
case "$_ref_line" in
    *'SPIRA_INCIDENT_REF="suite:$s"'*) ok "suites.sh keys on the suite alone" ;;
    '')  bad "suites.sh keys on the suite alone" "no SPIRA_INCIDENT_REF=\"suite:...\" line found" ;;
    *)   bad "suites.sh keys on the suite alone" "found [$_ref_line]" ;;
esac

# ======================================================================================
echo
echo "a red suite never escalates as a Sin:"
# ======================================================================================
# The timed run blocks nothing and files ordinary work. Left un-exempt, the per-suite key
# reaches SIN_AT recurrences quickly for any chronically red suite and pages the operator
# for something no page can act on (law-alerts-must-be-actionable).
grep -q 'SPIRA_SIN_EXEMPT=1' "$HERE/suites.sh" \
    && ok "suites.sh files reds sin-exempt" \
    || bad "suites.sh files reds sin-exempt" "no SPIRA_SIN_EXEMPT=1 beside the red filing"

echo
echo "test-suites-red-dedup.sh: $pass passed, $fail failed"
[ "$fail" -eq 0 ]
