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
echo "the concurrent case — two simultaneous filers of the same ref create exactly one bead:"
# ======================================================================================
# THE INCIDENT CASE, REPRODUCED. Both processes call open_incident before either calls
# bdq create; without the flock both get "no open incident" and both file. The lock in
# drain_one serialises the check-and-create pair so the second caller waits, re-runs
# open_incident inside the lock, finds the bead the first caller just created, and records
# a recurrence instead.
#
# Both processes are started with no sleep between them. Dolt's query latency is enough to
# widen the race window so this is not a lucky ordering — it is the same shape as the
# incident, just in a test database.
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

# A separate wrapper that accepts extra env vars as leading positional args (env(1)
# treats leading VAR=val tokens as environment assignments). Stdout is discarded; use
# find_bead to locate what was filed.
inc_env() {
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
        bash "$HERE/incident.sh" file "harness repo test" - >/dev/null 2>&1
}

# Find the bead by its external ref (the ref incident.sh derives from the title).
# NOTE: bd-embedded does not support --external-ref server-side filtering, so filter
# client-side via JSON.
find_bead() {   # find_bead <external-ref> -> bead id or empty
    bd -C "$SPIRA_DB" list --status open,in_progress --limit 0 --json 2>/dev/null \
      | python3 -c '
import sys, json
target = sys.argv[1]
try: d = json.load(sys.stdin)
except Exception: sys.exit(0)
for i in (d if isinstance(d, list) else [d]):
    if i.get("external_ref") == target:
        print(i["id"]); break
' "$1"
}

# ======================================================================================
echo
echo "the repo: label — a declared repo is stamped on the bead (positive control):"
# ======================================================================================
# POSITIVE CONTROL (law-absence-needs-a-positive-control). File an incident that names
# a repository and assert the resulting bead carries the repo: label.
# Without this, a mis-set SPIRA_DB, a broken label command, or an absent code path all
# look like "no label" to an assertion that only checks for its absence.
printf 'repo test payload\n' | inc_env SPIRA_INCIDENT_REPO=brain
id_repo="$(find_bead 'incident:harness-repo-test')"
if [ -n "${id_repo:-}" ]; then
    labels_repo="$(bd -C "$SPIRA_DB" label list "$id_repo" 2>/dev/null || true)"
    want "repo:brain on a bead filed with SPIRA_INCIDENT_REPO=brain" "repo:brain" "$labels_repo"
else
    bad "repo label: positive control" "incident.sh filed nothing (no bead at incident:harness-repo-test)"
fi

testdb_reset
mkdir -p "$RUN"
> "$ILOG"

# ======================================================================================
echo
echo "the repo: label — an undeclared repo is marked needs-repo-triage, not silently defaulted:"
# ======================================================================================
# WHERE THE CALLER DECLARES NO REPO the bead must carry needs-repo-triage rather than
# silently going to the home-repo fallback. A wrong repo is not indistinguishable from a
# right one (sp-io5e, law-a-split-repoints-nothing).
printf 'no-repo payload\n' | inc_env
id_norep="$(find_bead 'incident:harness-repo-test')"
if [ -n "${id_norep:-}" ]; then
    labels_norep="$(bd -C "$SPIRA_DB" label list "$id_norep" 2>/dev/null || true)"
    want "needs-repo-triage when no SPIRA_INCIDENT_REPO declared" "needs-repo-triage" "$labels_norep"
    case "$labels_norep" in
        *"repo:"*) bad "no-repo: must carry no repo: label when repo undeclared" "got: $labels_norep" ;;
        *) ok "no-repo: no repo: label present when repo undeclared" ;;
    esac
else
    bad "no-repo: positive control" "incident.sh filed nothing (no bead at incident:harness-repo-test)"
fi

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

# ======================================================================================
echo
echo "undeclared-repo escalation — a mail is sent when repo is not declared:"
# ======================================================================================
NOREP_MAIL="$TMP/norep-mail"
inc_norep() {
    env -i HOME="$HOME" PATH="$PATH" SPIRA_PATH="${SPIRA_PATH:-}" \
        SPIRA_CONF="$TMP/nonexistent.conf" \
        SPIRA_DB="$SPIRA_DB" \
        SPIRA_SPOOL="$SPOOL" \
        SPIRA_INCIDENT_LOG="$ILOG" \
        SPIRA_INCIDENT_LOCK="$LOCK" \
        SPIRA_RUN="$RUN" \
        SPIRA_HOME="$HERE" \
        SPIRA_MAIL="$NOREP_MAIL" \
        "$@" bash "$HERE/incident.sh" file "undeclared repo test" - >/dev/null 2>&1
}
n_norep_mails() { ls "$NOREP_MAIL/operator/new/" 2>/dev/null | wc -l | tr -d ' '; }
norep_mail_content() { cat "$NOREP_MAIL/operator/new/"* 2>/dev/null; }

echo
echo "  positive control — single filing sends one mail:"
printf 'first payload\n' | inc_norep
n="$(n_norep_mails)"
is "single undeclared-repo incident sends exactly one mail" "1" "$n"

testdb_reset; mkdir -p "$RUN"; > "$ILOG"

# ======================================================================================
echo
echo "provenance — undeclared-repo mail leads with unit+host+path, not the ref slug:"
# ======================================================================================
# THE REJECTED ASKS (sp-fzxk9, sp-dmjge). Four escalations in one hour were rejected as
# unreadable. The leading line was the external_ref slug — a dedupe key, not a sentence.
# "is this from a test container?" was asked three times. This verifies the fix:
# the mail subject now leads with "<unit> on <host>: <path>", making the origin unmistakable.
rm -rf "$NOREP_MAIL"
printf 'provenance payload\n' | inc_norep SPIRA_INCIDENT_PATH="$HERE/test-incident.sh"
_mail_subj="$(norep_mail_content | sed -n 's/^Subject:[[:space:]]*//p' | head -1)"
case "$_mail_subj" in
    "undeclared repo:"*) bad "provenance: mail subject starts with ref slug (not provenance)" "got: $_mail_subj" ;;
    *)                   ok "provenance: mail subject does not start with ref slug" ;;
esac
case "$_mail_subj" in
    *" on "*) ok "provenance: mail subject contains provenance marker ' on '" ;;
    *)        bad "provenance: mail subject must contain ' on '" "got: $_mail_subj" ;;
esac
case "$_mail_subj" in
    *"test-incident.sh"*) ok "provenance: mail subject contains the declared SPIRA_INCIDENT_PATH" ;;
    *)  bad "provenance: declared path (test-incident.sh) must appear in mail subject" "got: $_mail_subj" ;;
esac

testdb_reset; mkdir -p "$RUN"; > "$ILOG"

# ======================================================================================
echo
echo "dedup efficiency — O(1) bd show calls regardless of open-incident queue depth (sp-80br6):"
# ======================================================================================
# THE PROBLEM. The dedup path formerly called bd show once per candidate returned by
# bd list --label <labels>. N open incidents meant N sequential subprocess calls inside
# the intake flock, measured at ~202ms each: 10 open incidents added ~2s per new filing,
# exactly when the queue is deepest.
#
# THE FIX. Each bead now carries ref:<hash-of-external-ref> at filing time. _dedup_incident
# queries bd list --label ref:<hash>, returning at most 1 candidate, and reads external_ref
# directly from bd list --json output — no bd show per candidate.
#
# POSITIVE CONTROL (law-absence-needs-a-positive-control). The naive O(N) approach must
# visibly show N bd show calls for N candidates, proving the counter wrapper is working.
# A counter that always returns 0 would make any code look efficient; the positive control
# distinguishes "nothing calls bd show" from "the counter is broken."
#
# THE COUNTER WRAPS $SPIRA_BD. incident.sh uses bdq (which wraps $SPIRA_BD) for all bd
# calls; replacing SPIRA_BD with a counting wrapper captures every bd show invocation.
# The wrapper writes to SHOW_COUNT_FILE atomically enough for this single-process test.

SHOW_COUNT_FILE="$TMP/show-count"
BD_REAL="${SPIRA_BD:-bd}"

BD_COUNTER="$TMP/bd-counter"
# The wrapper must use the original bd binary ($BD_REAL), not itself recursively.
# Scan all args for 'show': bdq prepends -C $SPIRA_DB, so 'show' is not always at $1.
cat > "$BD_COUNTER" <<WRAPPER
#!/usr/bin/env bash
for _a in "\$@"; do
    if [ "\$_a" = "show" ]; then
        _c=\$(cat "$SHOW_COUNT_FILE" 2>/dev/null || echo 0)
        printf '%d\n' \$((_c+1)) > "$SHOW_COUNT_FILE"
        break
    fi
done
exec "$BD_REAL" "\$@"
WRAPPER
chmod +x "$BD_COUNTER"

# POSITIVE CONTROL: a naive O(N) function that calls bd show for each candidate in the
# open incident list — the shape of the OLD dedup path. Proves the counter captures shows.
_naive_dedup_show_count() {
    local search_labels
    search_labels="$(printf '%s' "${LABELS:-spira,incident}" | tr ',' '\n' | grep -v '^repo:' | paste -sd, -)"
    printf '0\n' > "$SHOW_COUNT_FILE"
    "$BD_COUNTER" -C "$SPIRA_DB" list --status open,in_progress --limit 0 \
        --label "$search_labels" --json 2>/dev/null \
      | python3 -c "
import sys, json, subprocess
bd_bin = sys.argv[1]
spira_db = sys.argv[2]
try:
    for bead in json.load(sys.stdin):
        bid = bead.get('id')
        if not bid: continue
        subprocess.run([bd_bin, '-C', spira_db, 'show', bid, '--json'], capture_output=True)
except: pass
" "$BD_COUNTER" "$SPIRA_DB" 2>/dev/null
    cat "$SHOW_COUNT_FILE" 2>/dev/null || echo 0
}

# Plant N beads with different refs so the naive loop has N candidates to bd-show.
N_BENCH=5
for _i in $(seq 1 $N_BENCH); do
    "$BD_REAL" -C "$SPIRA_DB" create "bench-incident-$_i" \
        --type bug --priority 2 --labels spira,incident \
        --external-ref "incident:bench-ref-$_i" --silent >/dev/null 2>&1
done

naive_shows="$(_naive_dedup_show_count)"
is "positive control: naive O(N) approach calls bd show $N_BENCH times for $N_BENCH candidates" \
   "$N_BENCH" "$naive_shows"

testdb_reset; mkdir -p "$RUN"; > "$ILOG"

# NEW CODE: file via incident.sh (which uses the label-keyed path) alongside N-1 noise
# beads that lack the ref: label. The dedup path on the second filing should issue
# exactly 0 bd show calls — the label query returns 1 candidate and external_ref is
# read directly from bd list --json output.
for _i in $(seq 2 $N_BENCH); do
    "$BD_REAL" -C "$SPIRA_DB" create "bench-noise-$_i" \
        --type bug --priority 2 --labels spira,incident \
        --external-ref "incident:noise-ref-$_i" --silent >/dev/null 2>&1
done

# File the target ref via incident.sh; it creates the bead and adds ref:<hash> label.
printf 'seed\n' | inc SPIRA_INCIDENT_REF="incident:bench-target" >/dev/null 2>&1 || true

# Second filing on same ref (the dedup recurrence path). Count bd show calls.
printf '0\n' > "$SHOW_COUNT_FILE"
printf 'recur\n' | SPIRA_BD="$BD_COUNTER" inc SPIRA_INCIDENT_REF="incident:bench-target" >/dev/null 2>&1 || true
new_shows="$(cat "$SHOW_COUNT_FILE" 2>/dev/null || echo 0)"
is "label-keyed dedup issues 0 bd show calls with $N_BENCH open candidates" "0" "$new_shows"

testdb_reset; mkdir -p "$RUN"; > "$ILOG"

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

echo
printf '%s: %d passed, %d failed\n' "$(basename "$0")" "$pass" "$fail"
[ "$fail" -eq 0 ]
