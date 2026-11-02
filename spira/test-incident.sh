#!/usr/bin/env bash
#
# test-incident.sh — the intake dedupe holds against two concurrent filers of the same ref.
#
#   ./test-incident.sh
#
# THE INCIDENT THIS SUITE REPRODUCES. The watchtower's first live run at 2026-09-07T22:30:15Z
# produced TWO beads for the same ref, identical to the second: sp-aapz and sp-uvfq. The
# log named both at the same timestamp, which is only possible if two invocations ran
# concurrently — the watchtower timer and the install-triggered start. The spool had drained
# both entries; both called open_incident before either called bdq create; both got "no open
# incident"; both filed.
#
# THE MECHANISM IS A CHECK FOLLOWED BY AN ACT. open_incident asks the database whether any
# open bead carries the ref; file_one creates one if not. Two callers that ask before either
# answers both get "no" and both file. That is not an atomic operation; only a mutual-
# exclusion lock around the entire check-and-create pair makes it one.
#
# THE FIX IS A FLOCK AROUND drain_one. Every path through the intake — `incident.sh file`,
# `incident.sh systemd`, `incident.sh drain` — passes through drain_one, so one lock guards
# all of them. The wait is bounded and a timeout leaves the entry spooled, which is the
# write-ahead behaving as designed.
#
# WHAT IS UNDER TEST HERE IS NOT THE SEQUENTIAL CASE. Sequential calls evidently already
# deduped; the incident was concurrent. A suite that planted one bead and then filed a second
# would test the sequential path and miss the race entirely. This suite plants NOTHING and
# fires two filers simultaneously against an empty database.
#
# THE POSITIVE CONTROL COMES FIRST (law-absence-needs-a-positive-control). Before asserting
# that concurrent calls dedupe, this suite verifies that a SINGLE call creates a bead and
# that OPEN_INCIDENT FINDS IT — using open_incident's own query path (the bd list
# --external-ref filter). A mis-set SPIRA_DB, a wrong label, or a broken external-ref filter
# all look like "nothing was created" to an assertion that only counts beads, and the control
# is what distinguishes them from "the lock works".
#
# Driven through the REAL incident.sh against a REAL bd on a throwaway fixture database —
# no stubs. The race is a claim about the database transaction ordering, and a stub that
# returned "no open bead" on every call would make a suite that always "passed"
# (law-prefer-the-real-dependency).
#
# defect: sp-5ll6
# tier: T2
# covers: spira/incident.sh watchtower/src/* incident/* UC-ops-detection-remediation-01 UC-ops-detection-remediation-02 UC-ops-detection-remediation-03
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"
tl_config SPIRA_TEST_CASE_DEADLINE=60

echo "test-incident.sh"

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-incident
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up incident || { echo "test-incident: could not build a fixture database"; exit 1; }
BD_REAL="${SPIRA_BD:-bd}"
# sp-jgjvh: incident beads are work beads, so the dedup reads each one's state from its
# lifecycle row, never bd status. With no lifecycle store here, a stand-in spira-lc tells
# this database's bd story in lifecycle terms (testlib.sh lc_mirror_bd: open → READY,
# closed → LANDED); a machine that does not answer would leave every filing spooled.
lc_mirror_bd "$TMP/lc"

# Create a minimal repo-map so bdq can validate repo: labels in the test.
# Format is name|path (pipe-separated), with comments starting with #.
REPO_MAP="$TMP/repo-map"
printf 'brain|%s\n' "$SPIRA_DB" > "$REPO_MAP"

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
    env -i HOME="$HOME" PATH="$PATH" \
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
echo "the concurrent case — two simultaneous filers of the same ref create exactly one bead:"
# ======================================================================================
# THE INCIDENT CASE, REPRODUCED. Both processes call open_incident before either calls
# bdq create; without the flock both get "no open incident" and both file. The lock in
# drain_one serialises the check-and-create pair so the second caller waits, re-runs
# open_incident inside the lock, finds the bead the first caller just created, and records
# a recurrence instead.
#
# A DETERMINISTIC BARRIER (gap G11), not a hope that Dolt's own query latency happens to
# widen the race window on whatever machine this runs on. SLOW_BD wraps the real bd binary
# and sleeps before every `create` call — inside the section the flock protects — so the
# second caller is guaranteed to still be waiting on the lock (or, if the lock were removed,
# guaranteed to overlap the first caller's create) regardless of how fast the fixture
# database answers today.
SLOW_BD="$TMP/slow-bd"
cat > "$SLOW_BD" <<WRAP
#!/usr/bin/env bash
for _a in "\$@"; do [ "\$_a" = create ] && sleep 0.3 && break; done
exec "$BD_REAL" "\$@"
WRAP
chmod +x "$SLOW_BD"
# Declared once, before either concurrent call, rather than inside inc()'s own per-call
# tl_config: both filers want the SAME value, and two processes racing a write to the same
# override.toml is exactly the kind of race this section exists to avoid, not reproduce.
tl_config SPIRA_BD="$SLOW_BD"
printf 'concurrent A\n' | inc >/dev/null &
pid_a=$!
printf 'concurrent B\n' | inc >/dev/null &
pid_b=$!
wait "$pid_a" || true
wait "$pid_b" || true

n="$(count_open 'incident:the-test-sweep')"
is "two concurrent filers create exactly one bead" "1" "$n"
recur_count="$(grep -c 'recurred' "$ILOG" 2>/dev/null || true)"
is "the second caller recorded a recurrence, not a second filing" "1" "$recur_count"

testdb_reset
mkdir -p "$RUN"
> "$ILOG"

# ======================================================================================
echo
echo "cross-repo dedup — same ref, different repo: labels resolve to ONE incident (sp-jvlrs):"
# ======================================================================================
# THE PROPERTY a61a110 HAD NO WAY TO OBSERVE. open_incident formerly filtered on the
# caller's full LABELS including repo:, so two filers declaring different repos produced
# disjoint candidate sets and each filed a fresh bead. The fix strips repo: from the dedup
# filter (DEDUPE_LABELS), making external_ref the effective key regardless of which repo
# the caller declared.
#
# POSITIVE CONTROL: one filing with repo:brain creates a bead. A second filing of the same
# title with repo:fixture-repo must find that bead and leave exactly one open bead.
_cross_title="cross repo dedup test"
_cross_ref="incident:cross-repo-dedup-test"
_cross_env() {
    tl_config SPIRA_PATH="${SPIRA_PATH:-}" SPIRA_DB="$SPIRA_DB" SPIRA_REPO_MAP="$REPO_MAP" \
        SPIRA_RUN="$RUN" SPIRA_MAIL="$TMP/mail"
    env -i HOME="$HOME" PATH="$PATH" \
        SPIRA_TOML="$SPIRA_TOML" \
        SPIRA_CONF="$TMP/nonexistent.conf" \
        SPIRA_SPOOL="$SPOOL" \
        SPIRA_INCIDENT_LOG="$ILOG" \
        SPIRA_INCIDENT_LOCK="$LOCK" \
        SPIRA_HOME="$HERE" \
        SPIRA_LC_BIN="$SPIRA_LC_BIN" \
        "$@" \
        incident.sh file "$_cross_title" - >/dev/null
}
printf 'first filer\n'  | _cross_env SPIRA_INCIDENT_REPO=brain
printf 'second filer\n' | _cross_env SPIRA_INCIDENT_REPO=fixture-repo
open_json="$(timeout 5 bd -C "$SPIRA_DB" list --status open,in_progress --limit 0 --json 2>/dev/null)"
n="$(printf '%s' "$open_json" | python3 -c '
import sys, json
target = sys.argv[1]; count = 0
try: d = json.load(sys.stdin)
except Exception: print(0); raise SystemExit(0)
for i in (d if isinstance(d, list) else [d]):
    if i.get("external_ref") == target: count += 1
print(count)
' "$_cross_ref")"
if [ "$n" != 1 ]; then
    echo "# intake log:"; sed 's/^/# /' "$ILOG"
    echo "# open rows for $_cross_ref:"
    printf '%s' "$open_json" | python3 -c '
import sys, json
try: d = json.load(sys.stdin)
except Exception: print("unparseable bd output"); raise SystemExit(0)
for i in (d if isinstance(d, list) else [d]):
    if i.get("external_ref") == sys.argv[1]: print(json.dumps(i))
' "$_cross_ref" | sed 's/^/# /'
fi
# recurrence logging is covered by the sequential case and run::tests::second_filing_is_a_recurrence_not_a_new_bead; flip-deleted r-auto-683
is "same ref with different repo: labels resolves to one incident, not two" "1" "$n"

testdb_reset
mkdir -p "$RUN"
> "$ILOG"

# UC-05 (declared repo:brain / undeclared needs-repo-triage + mail) and UC-04 (0 bd show
# calls under load) are demoted to T1 stub-bd rows in test-incident-decisions.sh — neither
# needs a real fixture database to be true. Only the fallback path below (a bead filed
# before the ref: label existed) stays here, exercising the real end-to-end label promotion.

# FALLBACK TEST: a bead filed without the ref: label (older code) still dedupes.
# The fallback path (sub-path B) handles this case correctly.
# Plant a bead manually (no ref: label, with the right external_ref).
_fb_ref="incident:fallback-test-ref"
_fb_id="$("$BD_REAL" -C "$SPIRA_DB" create "fallback test incident" \
    --type bug --priority 2 --labels spira,incident \
    --external-ref "$_fb_ref" --silent 2>/dev/null | tr -d '[:space:]')"
[ -n "$_fb_id" ] && \
    "$BD_REAL" -C "$SPIRA_DB" label add "$_fb_id" "sp-recur-1" >/dev/null 2>&1 || true

# File the same ref via incident.sh — must find the existing bead (recurrence, not new).
printf 'fallback recur\n' | inc SPIRA_INCIDENT_REF="$_fb_ref" >/dev/null 2>&1 || true
n_fb="$(timeout 5 bd -C "$SPIRA_DB" list --status open,in_progress --limit 0 --json 2>/dev/null \
  | python3 -c "
import sys, json
target = sys.argv[1]
count = 0
try: d = json.load(sys.stdin)
except Exception: sys.exit(0)
for i in (d if isinstance(d, list) else [d]):
    if i.get('external_ref') == target: count += 1
print(count)
" "$_fb_ref")"
is "a bead filed without ref: label is still found via the fallback path" "1" "$n_fb"
recur_fb="$(grep -c 'recurred' "$ILOG" 2>/dev/null || true)"
is "fallback-found bead was treated as recurrence, not new filing" "1" "$recur_fb"

testdb_reset; mkdir -p "$RUN"; > "$ILOG"

# ======================================================================================
echo
echo "external_ref in bd list --json — the field the dedup query reads must be present (sp-csvzn):"
# ======================================================================================
# THE DEFECT (sp-csvzn). bd list --json once omitted external_ref, so every candidate's
# bead.get('external_ref') was None and _dedup_incident always returned nothing — every
# filing looked like "no open incident" and filed a fresh bead (17-18+ surplus per ref,
# merged from test-incident-dedup.sh).
timeout 5 bd -C "$SPIRA_DB" create "external-ref field probe" \
    --type bug --priority 2 --labels spira,incident \
    --external-ref "probe:external-ref-field-check" --silent >/dev/null 2>&1
_has_field="$(timeout 5 bd -C "$SPIRA_DB" list --status open --limit 0 --json 2>/dev/null | python3 -c '
import sys, json
try:
    for r in json.load(sys.stdin):
        if r.get("external_ref") == "probe:external-ref-field-check":
            print("yes"); raise SystemExit(0)
    print("no")
except SystemExit: raise
except Exception:
    print("no")
')"
is "bd list --json includes external_ref with the correct value" "yes" "$_has_field"

testdb_reset; mkdir -p "$RUN"; > "$ILOG"

tl_summary
