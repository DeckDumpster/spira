#!/usr/bin/env bash
#
# test-watcher-reopen.sh — watcher-filed incidents respect deps and classify
# premature closes as closed-while-live.
#
# TWO PROPERTIES TESTED:
#
# 1. DEP PATH: a watcher incident with an open blocking dep stays open on the
#    next same-ref filing (no reopen event) and is absent from bd ready --claim.
#    This proves the mechanism that lets an Ops aeon link the incident to its
#    root cause and leave it open without triggering a fresh aeon on every tick.
#
# 2. CLOSED-WHILE-LIVE PATH: a bead closed and re-filed within WATCHER_INTERVAL_S
#    seconds receives reopen cause=closed-while-live rather than recurrence, so
#    census can measure this failure class separately.
#
# POSITIVE CONTROL (law-absence-needs-a-positive-control):
#   - Case 2 positive control: filing with WATCHER_INTERVAL_S=0 produces
#     cause=recurrence (the "outside interval" path fires); filing with a large
#     interval produces cause=closed-while-live. Both must fire.
#
# covers: spira/incident.sh spira/conf.sh spira/chamber/ops.md
# hermetic-ok: uses a fixture database, no systemd or gh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

echo "test-watcher-reopen.sh"

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-watcher-reopen
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up watcher_reopen || { echo "test-watcher-reopen: could not build fixture database" >&2; exit 1; }

INC="$HERE/incident.sh"
B() { bd -C "$SPIRA_DB" "$@"; }

RUN="$TMP/run"; mkdir -p "$RUN"
ILOG="$TMP/inc.log"

# Stub mail.sh so no real escalation fires.
mkdir -p "$TMP/home"
printf '#!/usr/bin/env bash\nexit 0\n' > "$TMP/home/mail.sh"; chmod +x "$TMP/home/mail.sh"

# file_watcher_incident <ref> <title> [VAR=val ...]
# Files a watcher-style incident (SIN_EXEMPT=1); output to /dev/null.
# Use find_bead "$ref" to get the bead id afterwards.
file_watcher_incident() {
    local ref="$1" title="$2"; shift 2
    printf 'watcher payload' | \
        env -i HOME="$HOME" PATH="$PATH" SPIRA_PATH="${SPIRA_PATH:-}" \
        SPIRA_CONF="$TMP/no-conf" \
        SPIRA_DB="$SPIRA_DB" \
        SPIRA_RUN="$RUN" \
        SPIRA_HOME="$TMP/home" \
        SPIRA_INCIDENT_LOG="$ILOG" \
        SPIRA_INCIDENT_REF="$ref" \
        SPIRA_INCIDENT_LOCK="$TMP/watcher.lock" \
        SPIRA_INCIDENT_REPO= \
        SPIRA_SIN_EXEMPT=1 \
        SPIRA_INCIDENT_CAUSE=watchtower \
        "$@" \
        bash "$INC" file "$title" - >/dev/null 2>&1
}

# find_bead <ref> — print bead id (any status) by external_ref, most recent first.
find_bead() {
    local ref="$1" hash
    hash="$(printf '%s' "$ref" | sha256sum | cut -c1-8)"
    # Open/in_progress first (fast path via label).
    local _id
    _id="$(B list --status open,in_progress --label "ref:$hash" --limit 0 --json 2>/dev/null \
      | python3 -c '
import sys, json
target = sys.argv[1]
try:
    for b in json.load(sys.stdin):
        if b.get("external_ref") == target:
            print(b["id"]); sys.exit(0)
except: pass
' "$ref" 2>/dev/null)"
    [ -n "$_id" ] && { printf '%s' "$_id"; return; }
    # Closed fallback.
    B list --status closed --closed-after "$(date -u -d '-1 day' '+%Y-%m-%d' 2>/dev/null \
        || date -u -v-1d '+%Y-%m-%d' 2>/dev/null)" \
      --label "ref:$hash" --limit 0 --json 2>/dev/null \
      | python3 -c '
import sys, json
target = sys.argv[1]
try:
    for b in json.load(sys.stdin):
        if b.get("external_ref") == target:
            print(b["id"]); sys.exit(0)
except: pass
' "$ref" 2>/dev/null
}

# bead_status <id> — print status of a bead.
bead_status() {
    B show "$1" --json 2>/dev/null \
      | python3 -c '
import sys, json
d = json.load(sys.stdin)
b = d if isinstance(d, dict) else (d[0] if d else {})
print(b.get("status", "?"))
' 2>/dev/null || printf '?'
}

# reopen_count <id> — count reopen events on the given bead.
reopen_count() {
    B sql "SELECT COUNT(*) FROM events WHERE issue_id='$1' AND event_type='reopen'" 2>/dev/null \
        | grep -v '^COUNT' | tr -d ' \t' | grep -E '^[0-9]+$' | head -1 || echo 0
}

# reopen_cause <id> — most recent reopen event's new_value (the cause).
reopen_cause() {
    B sql "SELECT new_value FROM events WHERE issue_id='$1' AND event_type='reopen' ORDER BY created_at DESC LIMIT 1" 2>/dev/null \
        | grep -v '^new_value' | tr -d ' \t' | grep -v '^$' | head -1 || echo ""
}

# ──────────────────────────────────────────────────────────────────────────────
# POSITIVE CONTROL — prove detection works in both directions before asserting
# absence. A check that finds nothing must first prove it could have found
# something (law-absence-needs-a-positive-control).
# ──────────────────────────────────────────────────────────────────────────────
echo ""
echo "# positive control"

# A watcher interval of 0 means no elapsed time qualifies → cause must be
# recurrence even for an immediately-refiled bead.
REF_PC1="incident:watcher-pc1-$$"
file_watcher_incident "$REF_PC1" "pc1" SPIRA_WATCHER_INTERVAL_S=0
PC1="$(find_bead "$REF_PC1")"
[ -n "$PC1" ] && ok "pc1: initial filing creates bead" || bad "pc1: initial filing creates bead" "no bead id"
B close "$PC1" --reason "test" >/dev/null 2>&1
file_watcher_incident "$REF_PC1" "pc1" SPIRA_WATCHER_INTERVAL_S=0
PC1_CAUSE="$(reopen_cause "$PC1")"
is "pc1: interval=0 → cause=recurrence" "recurrence" "$PC1_CAUSE"

# A large watcher interval means any recent close qualifies → cause=closed-while-live.
REF_PC2="incident:watcher-pc2-$$"
file_watcher_incident "$REF_PC2" "pc2" SPIRA_WATCHER_INTERVAL_S=9999
PC2="$(find_bead "$REF_PC2")"
[ -n "$PC2" ] && ok "pc2: initial filing creates bead" || bad "pc2: initial filing creates bead" "no bead id"
B close "$PC2" --reason "test" >/dev/null 2>&1
file_watcher_incident "$REF_PC2" "pc2" SPIRA_WATCHER_INTERVAL_S=9999
PC2_CAUSE="$(reopen_cause "$PC2")"
is "pc2: large interval → cause=closed-while-live" "closed-while-live" "$PC2_CAUSE"

# ──────────────────────────────────────────────────────────────────────────────
# CASE 1: DEP PATH — bead stays open when blocked by an open root bead.
# ──────────────────────────────────────────────────────────────────────────────
echo ""
echo "# case 1: dep path"

REF1="incident:watcher-dep-$$"

file_watcher_incident "$REF1" "watcher dep test"
INC1="$(find_bead "$REF1")"
[ -n "$INC1" ] && ok "case1: watcher incident filed" || bad "case1: watcher incident filed" "no bead id"

# Create an open root-cause bead and add it as a blocker.
ROOT1="$(B create "root cause for watcher dep test" -l spira,plan 2>/dev/null \
           | grep -oE '\b[a-z0-9]+-[a-z0-9]+\b' | head -1)"
[ -n "$ROOT1" ] && ok "case1: root cause bead created ($ROOT1)" \
                 || bad "case1: root cause bead created" "no id"

B dep add "$INC1" "$ROOT1" >/dev/null 2>&1
ok "case1: dep added (incident → root)"

# Refile the same ref (simulating next watcher tick). Bead is OPEN; should just note.
file_watcher_incident "$REF1" "watcher dep test"

# Assert: bead is still open (dep was not resolved; open bead path in incident.sh → no reopen).
S1="$(bead_status "$INC1")"
is "case1: bead stays open after refile with dep" "open" "$S1"

# Assert: no reopen event (bead was never closed, nothing to reopen).
R1="$(reopen_count "$INC1")"
is "case1: no reopen events" "0" "$R1"

# Assert: absent from bd ready (blocked by open dep).
READY1="$(B ready --json 2>/dev/null | python3 -c '
import sys, json
d = json.load(sys.stdin)
ids = [i.get("id","") for i in (d if isinstance(d, list) else [])]
print("absent" if "'"$INC1"'" not in ids else "present")
' 2>/dev/null)"
is "case1: blocked bead absent from bd ready" "absent" "$READY1"

# ──────────────────────────────────────────────────────────────────────────────
# CASE 2: CLOSED-WHILE-LIVE PATH — refile within interval → closed-while-live.
# ──────────────────────────────────────────────────────────────────────────────
echo ""
echo "# case 2: closed-while-live path"

REF2="incident:watcher-cwl-$$"

file_watcher_incident "$REF2" "watcher cwl test" SPIRA_WATCHER_INTERVAL_S=3600
INC2="$(find_bead "$REF2")"
[ -n "$INC2" ] && ok "case2: watcher incident filed" || bad "case2: watcher incident filed" "no bead id"

B close "$INC2" --reason "ops closed while condition held" >/dev/null 2>&1
S2_CLOSED="$(bead_status "$INC2")"
is "case2: bead closed" "closed" "$S2_CLOSED"

# Refile within the interval — expect cause=closed-while-live.
file_watcher_incident "$REF2" "watcher cwl test" SPIRA_WATCHER_INTERVAL_S=3600

S2_REOPENED="$(bead_status "$INC2")"
is "case2: bead reopened" "open" "$S2_REOPENED"

CAUSE2="$(reopen_cause "$INC2")"
is "case2: reopen cause=closed-while-live" "closed-while-live" "$CAUSE2"

# ──────────────────────────────────────────────────────────────────────────────
printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" = 0 ]
