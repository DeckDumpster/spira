#!/usr/bin/env bash
#
# test-incident-recur-cause.sh — sp-recur-N labels carry a cause suffix.
#
#   ./test-incident-recur-cause.sh
#
# THE PROPERTY THIS TESTS. A recurrence label must be sp-recur-N-<cause>, never bare
# sp-recur-N. Without a cause the corpus cannot be grouped by failure class, which is
# the prerequisite for Maechen naming a class before Ryan notices it.
#
# TWO SCENARIOS:
#
#   1. Default cause (SPIRA_INCIDENT_CAUSE unset): label is sp-recur-1-unrecorded.
#      A caller that does not know the cause must still record something — unrecorded is
#      explicit, not silent (control: the census groups unrecorded separately from any
#      named cause, so "we did not capture it" is still queryable).
#
#   2. Named cause (SPIRA_INCIDENT_CAUSE=suite-red): label is sp-recur-1-suite-red, then
#      sp-recur-2-suite-red on the recurrence. counter_causes must return cause=suite-red.
#
# 3. WATCHER-FILED INCIDENTS (merged from test-watcher-reopen.sh): a bead blocked by an
#    open dep stays open on refile with no reopen event, and pc1/pc2 prove the interval
#    classifier both ways over a real bead (case2, which only re-ran pc2's exact scenario,
#    was deleted rather than merged — same assertion, no new fact).
#
# WHAT MOVED OUT (sp-fhzib.2, UC-ops-detection-remediation-07/07b): the backfill-recur-causes
# scenario is now a T1 stub-bd row in test-incident-migrations.sh (that migration needs no
# database to be true), and the Sin-escalation scenario duplicated test-sin-exempt.sh exactly
# and is deleted rather than ported. What remains here is UC-operator-channel-36's own scope.
#
# A REAL bd ON A FIXTURE DATABASE (law-prefer-the-real-dependency), except the classifier
# itself (section 0 below), which is pure and needs no database at all — a T1 seam inside a
# suite whose other sections stay T3 because the events table is the fact under test there.
#
# covers: spira/incident.sh spira/lib.sh UC-operator-channel-36
# tier: T3
# hermetic-ok: uses a fixture database, no systemd or gh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

echo "test-incident-recur-cause.sh"

# ======================================================================================
echo
echo "0. _reopen_cause <closed-epoch> <now-epoch> <interval-s> — pure, no database:"
# ======================================================================================
# SPIRA_INCIDENT_CAUSE set only to keep incident.sh's own "cause not set" warning (a
# top-level check unrelated to what this section tests) out of the suite's output.
export SPIRA_INCIDENT_CAUSE=classifier-probe
# shellcheck disable=SC1090
. "$HERE/incident.sh"
is "well inside the interval: closed-while-live" "closed-while-live" "$(_reopen_cause 1000 1100 200)"
is "past the interval: recurrence"               "recurrence"        "$(_reopen_cause 1000 1300 200)"
is "exactly at the interval boundary: recurrence (not < counts as outside)" \
    "recurrence" "$(_reopen_cause 1000 1200 200)"
is "interval=0 never qualifies as live: recurrence" "recurrence" "$(_reopen_cause 1000 1000 0)"
is "a large interval still holds far out: closed-while-live" \
    "closed-while-live" "$(_reopen_cause 1000 10998 9999)"

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-incident-recur-cause
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
# testdb-mode: server — asserts on recurs_of, which reads the events table via bd sql
export SPIRA_TESTDB_MODE=server
testdb_up rc_cause || {
    printf 'SKIP test-incident-recur-cause: server testdb not available\n' >&2
    exit 77
}

# lib.sh is sourced for counter_of / counter_causes / recur_causes.
# shellcheck disable=SC1090
. "$HERE/lib.sh"
INC="$HERE/incident.sh"
B() { bd -C "$SPIRA_DB" "$@"; }
mkdir -p "$TMP/run"

# Stub mail.sh so no real escalation fires but sends are recorded.
mkdir -p "$TMP/inc-home"
export MAIL_LOG="$TMP/mail.log"
cat > "$TMP/inc-home/mail.sh" <<'M'
#!/usr/bin/env bash
[ "${1:-}" = send ] || exit 0
printf '%s\n' "$*" >> "$MAIL_LOG"
cat >> "$MAIL_LOG"
M
chmod +x "$TMP/inc-home/mail.sh"

file_incident() {  # file_incident <ref> <title> <payload> [VAR=val ...]
    local ref="$1" title="$2" payload="$3"; shift 3
    printf '%s' "$payload" | \
        env SPIRA_DB="$SPIRA_DB" SPIRA_RUN="$TMP/run" SPIRA_CONF="$TMP/no-conf" \
        SPIRA_HOME="$TMP/inc-home" \
        SPIRA_INCIDENT_REF="$ref" \
        SPIRA_INCIDENT_LOCK="$TMP/run/rc-cause-test.lock" \
        SPIRA_INCIDENT_REPO= \
        "$@" \
        bash "$INC" file "$title" - 2>/dev/null
}

# --- helpers for section 3/4 (merged from test-watcher-reopen.sh) -----------------------

# file_watcher_incident <ref> <title> [VAR=val ...] — files a watcher-style incident
# (SIN_EXEMPT=1). Uses env (not env -i) to preserve SPIRA_BD so the subprocess's bdq uses
# the server-mode binary and can write to the events table via bd sql.
file_watcher_incident() {
    local ref="$1" title="$2"; shift 2
    printf 'watcher payload' | \
        env SPIRA_CONF="$TMP/no-conf" \
        SPIRA_DB="$SPIRA_DB" \
        SPIRA_RUN="$TMP/run" \
        SPIRA_HOME="$TMP/inc-home" \
        SPIRA_INCIDENT_REF="$ref" \
        SPIRA_INCIDENT_LOCK="$TMP/run/watcher.lock" \
        SPIRA_INCIDENT_REPO= \
        SPIRA_SIN_EXEMPT=1 \
        SPIRA_INCIDENT_CAUSE=watchtower \
        "$@" \
        bash "$INC" file "$title" - >/dev/null 2>&1
}

# find_bead <ref> — print bead id (open, in_progress or closed) by external_ref. Uses the
# same broad spira,partition:incident label query the rest of this file already relies on
# (sections 1/2/5), rather than the ref:<hash> label-keyed lookup incident.sh itself uses
# internally — that path is what is under test here, so this helper does not depend on it.
find_bead() {
    local ref="$1" _id
    for _status in open in_progress closed; do
        _id="$(B list --status "$_status" --limit 0 --label spira,partition:incident --json 2>/dev/null \
          | python3 -c '
import sys, json
target = sys.argv[1]
try: d = json.load(sys.stdin)
except: sys.exit(0)
d = d if isinstance(d, list) else [d]
for b in d:
    if b.get("external_ref") == target:
        print(b["id"]); break
' "$ref" 2>/dev/null)"
        [ -n "$_id" ] && { printf '%s' "$_id"; return; }
    done
}

bead_status() {
    B show "$1" --json 2>/dev/null \
      | python3 -c '
import sys, json
d = json.load(sys.stdin)
b = d if isinstance(d, dict) else (d[0] if d else {})
print(b.get("status", "?"))
' 2>/dev/null || printf '?'
}

# sql_val — parse bd sql output: skip header+separator (lines 1-2), return first data line.
sql_val() { tail -n +3 | head -1 | tr -d ' '; }

# sql_reopen_cause <id> — most recent reopen event's new_value (the cause), read straight
# from the events table — the fact _reopen_cause's caller actually wrote, not a re-derivation.
sql_reopen_cause() {
    B sql "SELECT new_value FROM events WHERE issue_id='$1' AND event_type='reopen' ORDER BY created_at DESC LIMIT 1" 2>/dev/null \
        | sql_val | grep -v '^[(]' || echo ""
}

# sql_reopen_count <id> — count of "reopen" events (bead_reopen calls only, never fired for
# a refile of a bead that stayed open) — distinct from recurs_of's "recurred" events, which
# fire on every second-and-later filing whether or not the bead was ever closed.
sql_reopen_count() {
    B sql "SELECT COUNT(*) FROM events WHERE issue_id='$1' AND event_type='reopen'" 2>/dev/null \
        | sql_val | grep -E '^[0-9]+$' || echo 0
}

# ======================================================================================
echo
# sp-recur-N-<cause> labels are no longer written (sp-lzt); recurrences are events.
# The assertions in sections 1 and 2 that checked for those labels have been removed:
#   deleted: "initial filing carries sp-recur-1-unrecorded" (label no longer written)
#   deleted: "initial filing carries sp-recur-1-suite-red" (label no longer written)
#   deleted: "recurrence counter advanced to 2" via recur_max (recur_max reads labels)
#   deleted: "recurrence carries sp-recur-2-suite-red" (label no longer written)
#   deleted: "recur_causes reports suite-red" (recur_causes reads labels, now always empty)
# The replacement properties — bead creation and events-based recurrence count — are below.
echo "1. default cause (SPIRA_INCIDENT_CAUSE unset) — bead created, no recurrence event on first filing:"
# ======================================================================================
testdb_reset; mkdir -p "$TMP/run"
ref="incident:test-default-cause"
file_incident "$ref" "default cause test" "payload 1" >/dev/null

bid="$(B list --status open --limit 0 --label spira,partition:incident --json 2>/dev/null \
    | python3 -c '
import json,sys
target=sys.argv[1]
try: d=json.load(sys.stdin)
except: sys.exit(0)
d=d if isinstance(d,list) else [d]
for i in d:
    if i.get("external_ref")==target: print(i["id"]); break
' "$ref" 2>/dev/null)"
[ -n "$bid" ] && ok "bead was created for default-cause ref" \
    || { bad "bead was created for default-cause ref" "none found"; tl_summary; exit; }

is "first filing has no recurrence event (only recurrences write events)" "0" "$(recurs_of "$bid")"

# ======================================================================================
echo
echo "2. named cause (suite-red) — bead created, second filing produces one recurrence event:"
# ======================================================================================
testdb_reset; mkdir -p "$TMP/run"; > "$MAIL_LOG"
ref2="incident:test-named-cause"
file_incident "$ref2" "named cause test" "payload 1" SPIRA_INCIDENT_CAUSE=suite-red >/dev/null
file_incident "$ref2" "named cause test" "payload 2" SPIRA_INCIDENT_CAUSE=suite-red >/dev/null

bid2="$(B list --status open --limit 0 --label spira,partition:incident --json 2>/dev/null \
    | python3 -c '
import json,sys
target=sys.argv[1]
try: d=json.load(sys.stdin)
except: sys.exit(0)
d=d if isinstance(d,list) else [d]
for i in d:
    if i.get("external_ref")==target: print(i["id"]); break
' "$ref2" 2>/dev/null)"
[ -n "$bid2" ] && ok "bead was created for named-cause ref" \
    || { bad "bead was created for named-cause ref" "none found"; tl_summary; exit; }

is "second filing produces exactly one recurrence event" "1" "$(recurs_of "$bid2")"

# ======================================================================================
echo
echo "3. dep path — a watcher incident blocked by an open dep stays open on refile:"
# ======================================================================================
# Merged from test-watcher-reopen.sh: proves the mechanism that lets an Ops aeon link the
# incident to its root cause and leave it open without a fresh reopen event on every tick.
testdb_reset; mkdir -p "$TMP/run"

REF1="incident:watcher-dep-$$"
file_watcher_incident "$REF1" "watcher dep test"
INC1="$(find_bead "$REF1")"
[ -n "$INC1" ] && ok "watcher incident filed" || bad "watcher incident filed" "no bead id"

ROOT1="$(B create "root cause for watcher dep test" -l spira,plan 2>/dev/null \
           | grep -oE '\b[a-z0-9]+-[a-z0-9]+\b' | head -1)"
[ -n "$ROOT1" ] && ok "root cause bead created ($ROOT1)" || bad "root cause bead created" "no id"
B dep add "$INC1" "$ROOT1" >/dev/null 2>&1

# Refile the same ref (simulating next watcher tick). Bead is OPEN; should just note.
file_watcher_incident "$REF1" "watcher dep test"

is "bead stays open after refile with an open dep" "open" "$(bead_status "$INC1")"
is "no reopen event (bead was never closed)" "0" "$(sql_reopen_count "$INC1")"

READY1="$(B ready --json 2>/dev/null | python3 -c '
import sys, json
d = json.load(sys.stdin)
ids = [i.get("id","") for i in (d if isinstance(d, list) else [])]
print("absent" if "'"$INC1"'" not in ids else "present")
' 2>/dev/null)"
is "blocked bead absent from bd ready" "absent" "$READY1"

# ======================================================================================
echo
echo "4. the classifier over a real bead — pc1/pc2 positive control (both directions):"
# ======================================================================================
# law-absence-needs-a-positive-control: prove the classifier fires BOTH ways over a real
# close-then-refile before trusting anything it says elsewhere. (test-watcher-reopen.sh's
# case2 only re-ran pc2's exact scenario — deleted rather than merged, D-row 36.)
testdb_reset; mkdir -p "$TMP/run"

REF_PC1="incident:watcher-pc1-$$"
file_watcher_incident "$REF_PC1" "pc1" SPIRA_WATCHER_INTERVAL_S=0
PC1="$(find_bead "$REF_PC1")"
[ -n "$PC1" ] && ok "pc1: initial filing creates bead" || bad "pc1: initial filing creates bead" "no bead id"
B close "$PC1" --reason "test" >/dev/null 2>&1
file_watcher_incident "$REF_PC1" "pc1" SPIRA_WATCHER_INTERVAL_S=0
is "pc1: interval=0 → cause=recurrence" "recurrence" "$(sql_reopen_cause "$PC1")"

REF_PC2="incident:watcher-pc2-$$"
file_watcher_incident "$REF_PC2" "pc2" SPIRA_WATCHER_INTERVAL_S=9999
PC2="$(find_bead "$REF_PC2")"
[ -n "$PC2" ] && ok "pc2: initial filing creates bead" || bad "pc2: initial filing creates bead" "no bead id"
B close "$PC2" --reason "test" >/dev/null 2>&1
file_watcher_incident "$REF_PC2" "pc2" SPIRA_WATCHER_INTERVAL_S=9999
is "pc2: large interval → cause=closed-while-live" "closed-while-live" "$(sql_reopen_cause "$PC2")"

tl_summary
