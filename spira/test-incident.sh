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
# covers: spira/incident.sh spira/watchtower.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()  { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want(){ [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }

echo "test-incident.sh"

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-incident
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up incident || { echo "test-incident: could not build a fixture database"; exit 1; }
BD_REAL="${SPIRA_BD:-bd}"

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
    env -i HOME="$HOME" PATH="$PATH" SPIRA_PATH="${SPIRA_PATH:-}" \
        SPIRA_CONF="$TMP/nonexistent.conf" \
        SPIRA_DB="$SPIRA_DB" \
        SPIRA_SPOOL="$SPOOL" \
        SPIRA_INCIDENT_LOG="$ILOG" \
        SPIRA_INCIDENT_LOCK="$LOCK" \
        SPIRA_RUN="$RUN" \
        SPIRA_HOME="$HERE" \
        SPIRA_MAIL="$TMP/mail" \
        "$@" bash "$HERE/incident.sh" file "the test sweep" -
}

# Count open beads carrying the given external ref on the fixture database.
# NOTE: bd-embedded does not support --external-ref server-side filtering, so filter
# client-side via JSON, exactly as incident.sh open_incident does.
count_open() {
    bd -C "$SPIRA_DB" list --status open,in_progress --limit 0 --label spira,partition:incident --json 2>/dev/null \
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
    bd -C "$SPIRA_DB" list --all --status open,in_progress,closed --limit 0 --label spira,partition:incident --json 2>/dev/null \
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
echo "the positive control — a single call creates one bead and open_incident finds it:"
# ======================================================================================
# If this fails, open_incident's query (--external-ref, --label spira,incident) is not
# finding what incident.sh filed, and every concurrent-case assertion below is testing the
# wrong thing.
printf 'positive control payload\n' | inc >/dev/null
n="$(count_open 'incident:the-test-sweep')"
is "a single filing creates exactly one bead" "1" "$n"

testdb_reset
mkdir -p "$RUN"

# ======================================================================================
echo
echo "the sequential case — a second call on the same ref is a recurrence, not a new bead:"
# ======================================================================================
# The sequential dedupe evidently already passed before the incident. This case is kept as
# a sanity check: if it breaks, the lock (which is only needed for concurrency) is not the
# culprit and the external-ref filter is probably broken.
printf 'first\n' | inc >/dev/null
printf 'second\n' | inc >/dev/null
n="$(count_open 'incident:the-test-sweep')"
is "two sequential calls leave exactly one open bead" "1" "$n"
log_count="$(grep -c 'recurred' "$ILOG" 2>/dev/null || true)"
is "the second call was recorded as a recurrence, not a filing" "1" "$log_count"

testdb_reset
mkdir -p "$RUN"
> "$ILOG"

# ======================================================================================
echo
echo "close-then-refile dedup — a closed bead within the lookback is reopened (sp-srgr6):"
# ======================================================================================
# THE DEFECT REPRODUCED. open_incident formerly filtered --status open,in_progress only.
# Closing a bead for a still-failing ref made it invisible: the next invocation found no
# open bead and filed a fresh one — repeating until 38 duplicate beads accumulated in 2 days.
#
# SEEN TO FAIL against the unfixed tree (law-a-regression-test-must-be-seen-to-fail):
# Filing first bead; closing it; filing same ref again produced:
#   2026-09-10T14:25:47Z incident: filed sp-7ed for incident:the-test-sweep   ← second bead
# count_all returned 2 (one closed original + one fresh open bead).
#
# After the fix: recent_closed_incident finds the closed bead within SPIRA_INCIDENT_DEDUP_LOOKBACK
# days and file_one reopens it with a recurrence note — count_all stays 1, count_open stays 1.
printf 'first payload\n' | inc >/dev/null
# Extract and close the bead that was just filed.
_ctr_id="$(bd -C "$SPIRA_DB" list --status open,in_progress --limit 0 --json 2>/dev/null \
  | python3 -c '
import sys, json
target = sys.argv[1]
try: d = json.load(sys.stdin)
except Exception: sys.exit(0)
for i in (d if isinstance(d, list) else [d]):
    if i.get("external_ref") == target:
        print(i["id"]); break
' 'incident:the-test-sweep')"
[ -n "${_ctr_id:-}" ] && \
    bd -C "$SPIRA_DB" close "$_ctr_id" --reason "resolved in regression test" >/dev/null 2>&1 || true
> "$ILOG"
# File the same ref again — should reopen, not create a second bead.
printf 'second payload\n' | inc >/dev/null
n_all="$(count_all 'incident:the-test-sweep')"
is "close-then-refile leaves exactly one bead total (original reopened)" "1" "$n_all"
n_open="$(count_open 'incident:the-test-sweep')"
is "the original bead was reopened (now open)" "1" "$n_open"
log_reopen="$(grep -c 'reopened from closed' "$ILOG" 2>/dev/null || true)"
is "reopen path was logged, not a new filing" "1" "$log_reopen"

testdb_reset
mkdir -p "$RUN"
> "$ILOG"

# ======================================================================================
echo
echo "BSD date -v fallback — the lookback boundary still resolves without GNU date -d:"
# ======================================================================================
# _dedup_incident computes its lookback boundary with GNU \`date -d\`, falling back to BSD's
# \`date -v\` when -d is unsupported. A PATH with no GNU date must not silently skip the
# closed-bead lookback (which would make every close-then-refile file a fresh bead) — it
# must still resolve a boundary date via the BSD form.
# THE HOST HAS NO REAL BSD date — GNU date does not implement -v at all, so a shim that
# merely forwarded -v to the real binary would fail identically to -d and prove nothing.
# It translates BSD's `-v -Nd` into GNU's `-d "-N days"` against the same underlying
# binary, so the shim is what emulates BSD, not what happens to work by accident.
_gnu_date="$(command -v date)"
DATEDIR="$TMP/no-gnu-date"; mkdir -p "$DATEDIR"
cat > "$DATEDIR/date" <<DATESHIM
#!/usr/bin/env bash
# Reject GNU-style -d, forcing the caller's own fallback branch.
for _a in "\$@"; do [ "\$_a" = "-d" ] && { echo "date: illegal option -- d" >&2; exit 1; }; done
out=()
while [ "\$#" -gt 0 ]; do
    if [ "\$1" = "-v" ]; then
        shift
        off="\$1"; shift
        sign="\${off%%[0-9]*}"; rest="\${off#"\$sign"}"
        num="\${rest%%[a-zA-Z]*}"; unit="\${rest##*[0-9]}"
        case "\$unit" in
            d) word=days ;; H) word=hours ;; M) word=minutes ;; S) word=seconds ;;
            m) word=months ;; y) word=years ;; w) word=weeks ;;
            *) word=days ;;
        esac
        [ "\$sign" = "+" ] && sign=""
        out+=(-d "\${sign}\${num} \${word}")
    else
        out+=("\$1"); shift
    fi
done
exec "$_gnu_date" "\${out[@]}"
DATESHIM
chmod +x "$DATEDIR/date"
_bsd_since="$(PATH="$DATEDIR:$PATH" bash -c 'date -u -v -7d "+%Y-%m-%d" 2>/dev/null')"
is "the BSD date -v shim itself resolves a boundary" "yes" "$([ -n "$_bsd_since" ] && echo yes || echo no)"

printf 'bsd-date first payload\n' | PATH="$DATEDIR:$PATH" inc >/dev/null
_bd_id="$(bd -C "$SPIRA_DB" list --status open,in_progress --limit 0 --json 2>/dev/null \
  | python3 -c '
import sys, json
target = sys.argv[1]
try: d = json.load(sys.stdin)
except Exception: sys.exit(0)
for i in (d if isinstance(d, list) else [d]):
    if i.get("external_ref") == target:
        print(i["id"]); break
' 'incident:the-test-sweep')"
[ -n "${_bd_id:-}" ] && bd -C "$SPIRA_DB" close "$_bd_id" --reason "resolved, within lookback" >/dev/null 2>&1
> "$ILOG"
printf 'bsd-date second payload\n' | PATH="$DATEDIR:$PATH" inc >/dev/null
n_bsd_all="$(count_all 'incident:the-test-sweep')"
is "with no GNU date -d, the BSD fallback still finds the closed bead within lookback" "1" "$n_bsd_all"
log_bsd_reopen="$(grep -c 'reopened from closed' "$ILOG" 2>/dev/null || true)"
is "and reopens it rather than filing fresh" "1" "$log_bsd_reopen"

testdb_reset
mkdir -p "$RUN"
> "$ILOG"

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
printf 'concurrent A\n' | inc SPIRA_BD="$SLOW_BD" >/dev/null &
pid_a=$!
printf 'concurrent B\n' | inc SPIRA_BD="$SLOW_BD" >/dev/null &
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
# title with repo:fixture-repo must find that bead and record a recurrence.
_cross_title="cross repo dedup test"
_cross_ref="incident:cross-repo-dedup-test"
_cross_env() {
    env -i HOME="$HOME" PATH="$PATH" SPIRA_PATH="${SPIRA_PATH:-}" \
        SPIRA_CONF="$TMP/nonexistent.conf" \
        SPIRA_DB="$SPIRA_DB" \
        SPIRA_REPO_MAP="$REPO_MAP" \
        SPIRA_SPOOL="$SPOOL" \
        SPIRA_INCIDENT_LOG="$ILOG" \
        SPIRA_INCIDENT_LOCK="$LOCK" \
        SPIRA_RUN="$RUN" \
        SPIRA_HOME="$HERE" \
        SPIRA_MAIL="$TMP/mail" \
        "$@" \
        bash "$HERE/incident.sh" file "$_cross_title" - >/dev/null 2>&1
}
printf 'first filer\n'  | _cross_env SPIRA_INCIDENT_REPO=brain
printf 'second filer\n' | _cross_env SPIRA_INCIDENT_REPO=fixture-repo
n="$(bd -C "$SPIRA_DB" list --status open,in_progress --limit 0 --json 2>/dev/null \
  | python3 -c '
import sys, json
target = sys.argv[1]; count = 0
try: d = json.load(sys.stdin)
except Exception: print(0); raise SystemExit(0)
for i in (d if isinstance(d, list) else [d]):
    if i.get("external_ref") == target: count += 1
print(count)
' "$_cross_ref")"
is "same ref with different repo: labels resolves to one incident, not two" "1" "$n"
recur_cross="$(grep -c 'recurred' "$ILOG" 2>/dev/null || true)"
is "second cross-repo filer was recorded as a recurrence, not a new filing" "1" "$recur_cross"

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
n_fb="$(bd -C "$SPIRA_DB" list --status open,in_progress --limit 0 --json 2>/dev/null \
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
bd -C "$SPIRA_DB" create "external-ref field probe" \
    --type bug --priority 2 --labels spira,partition:incident \
    --external-ref "probe:external-ref-field-check" --silent >/dev/null 2>&1
_has_field="$(bd -C "$SPIRA_DB" list --status open --limit 0 --json 2>/dev/null | python3 -c '
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

# ======================================================================================
echo
echo "dedup with SPIRA_INCIDENT_REF set — the explicit-ref path resolves against \$SPIRA_DB, not a literal (sp-ew54u):"
# ======================================================================================
# THE DEFECT (sp-ew54u, merged from test-incident-dedup.sh). \$SPIRA_DB inside a
# single-quoted Python heredoc was never shell-expanded, so the query addressed the
# caller's default store instead of the configured one and every filing found nothing.
# The current code passes it as a sys.argv positional, never inside a Python string
# literal; this exercises exactly that path.
EXPLICIT_REF="incident:explicit-ref-dedup-test"
for _i in 1 2 3; do
    printf 'explicit ref filing %d\n' "$_i" | inc SPIRA_INCIDENT_REF="$EXPLICIT_REF" >/dev/null
done
n_explicit="$(count_open "$EXPLICIT_REF")"
is "3 filings with SPIRA_INCIDENT_REF produce exactly one bead (sp-ew54u)" "1" "$n_explicit"

testdb_reset; mkdir -p "$RUN"; > "$ILOG"

# ======================================================================================
echo
echo "recurrence notes are bounded for an unchanged payload (UC-06, one real row — the decision itself is T1 above):"
# ======================================================================================
# The pure note-size decision (_recur_note_body) is table-tested with no database in
# test-incident-decisions.sh; this is the one real row proving the wiring — that file_one
# actually calls it and that the resulting bead's notes field reflects the decision.
REF_BOUND="incident:test-recur-bounded"
PAYLOAD500="$(python3 -c 'print("x" * 500, end="")')"
for _i in 1 2 3; do
    printf '%s' "$PAYLOAD500" | inc SPIRA_INCIDENT_REF="$REF_BOUND" >/dev/null
done
_bound_id="$(bd -C "$SPIRA_DB" list --status open,in_progress --limit 0 --json 2>/dev/null \
  | python3 -c '
import sys, json
target = sys.argv[1]
try: d = json.load(sys.stdin)
except Exception: sys.exit(0)
for i in (d if isinstance(d, list) else [d]):
    if i.get("external_ref") == target: print(i["id"]); break
' "$REF_BOUND")"
[ -n "${_bound_id:-}" ] || bad "recur-bounded bead was created" "none found"
# The first recurrence has no prior payload-hash label, so it writes the payload once; the
# second recurrence's payload is byte-identical, so it must NOT write it again — that is the
# whole property _recur_note_body exists to enforce. A copy count of exactly 1 (not 0, not 2)
# proves both halves at once: the payload was recorded at all, and it was not re-recorded.
_payload_copies="$(bd -C "$SPIRA_DB" show "${_bound_id:-?}" --json 2>/dev/null | python3 -c '
import sys, json
try:
    d = json.load(sys.stdin); d = d if isinstance(d, list) else [d]
    notes = (d[0].get("notes") or "") if d else ""
    print(notes.count("x" * 500))
except Exception: print(-1)
')"
is "an unchanged 500-byte payload is recorded exactly once across 3 identical filings" "1" "$_payload_copies"

echo
printf '%s: %d passed, %d failed\n' "$(basename "$0")" "$pass" "$fail"
[ "$fail" -eq 0 ]
