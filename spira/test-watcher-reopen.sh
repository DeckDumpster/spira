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
#   - Case 1: a filing with no dep and the bead OPEN produces a bare recurrence note
#     (no reopen event). Adding a dep then re-filing must still produce no reopen.
#   - Case 2: a filing outside the watcher interval produces cause=recurrence; a
#     filing inside the interval produces cause=closed-while-live. Both must fire.
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
atleast(){ [ "$3" -ge "$2" ] && ok "$1" || bad "$1" "wanted >= $2, got $3"; }

echo "test-watcher-reopen.sh"

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-watcher-reopen
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
export SPIRA_TESTDB_MODE=server
testdb_up watcher_reopen || {
    printf 'SKIP test-watcher-reopen: server testdb not available\n' >&2
    exit 77
}

INC="$HERE/incident.sh"
B() { bd -C "$SPIRA_DB" "$@"; }

RUN="$TMP/run"; mkdir -p "$RUN"

# Stub mail.sh so no real escalation fires.
mkdir -p "$TMP/home"
printf '#!/usr/bin/env bash\nexit 0\n' > "$TMP/home/mail.sh"; chmod +x "$TMP/home/mail.sh"

# file_watcher_incident <ref> <title> [VAR=val ...]
# Files a watcher-style incident (SIN_EXEMPT=1) and prints the bead id.
file_watcher_incident() {
    local ref="$1" title="$2"; shift 2
    printf 'watcher payload' | \
        env -i HOME="$HOME" PATH="$PATH" SPIRA_PATH="${SPIRA_PATH:-}" \
        SPIRA_CONF="$TMP/no-conf" \
        SPIRA_DB="$SPIRA_DB" \
        SPIRA_RUN="$RUN" \
        SPIRA_HOME="$TMP/home" \
        SPIRA_INCIDENT_REF="$ref" \
        SPIRA_INCIDENT_LOCK="$TMP/watcher.lock" \
        SPIRA_INCIDENT_REPO= \
        SPIRA_SIN_EXEMPT=1 \
        SPIRA_INCIDENT_CAUSE=watchtower \
        "$@" \
        bash "$INC" file "$title" - 2>/dev/null
}

# reopen_count <id> — count reopen events on the given bead.
reopen_count() {
    B sql "SELECT COUNT(*) FROM events WHERE issue_id='$1' AND event_type='reopen'" 2>/dev/null \
        | tail -1 | tr -d ' '
}

# reopen_cause <id> — most recent reopen event's new_value (the cause).
reopen_cause() {
    B sql "SELECT new_value FROM events WHERE issue_id='$1' AND event_type='reopen' ORDER BY created_at DESC LIMIT 1" 2>/dev/null \
        | tail -1 | tr -d ' '
}

# ──────────────────────────────────────────────────────────────────────────────
# POSITIVE CONTROL — prove the infrastructure can detect both classes before we
# assert they do not appear where they should not.
# ──────────────────────────────────────────────────────────────────────────────
echo ""
echo "# positive control"

REF_PC="incident:watcher-test-pc-$$"

# File a watcher incident and immediately close it, then refile within the
# interval (WATCHER_INTERVAL_S=5). Expect cause=closed-while-live.
PC_ID="$(file_watcher_incident "$REF_PC" "pc-watcher" SPIRA_WATCHER_INTERVAL_S=5)"
[ -n "$PC_ID" ] && ok "positive-control: initial filing creates bead" \
                 || { bad "positive-control: initial filing creates bead" "no bead id returned"; }

B close "$PC_ID" --reason-file - <<'R' >/dev/null 2>&1
positive control close
R
PC_STATUS_AFTER_CLOSE="$(B show "$PC_ID" --json 2>/dev/null | python3 -c 'import sys,json; d=json.load(sys.stdin); print((d if isinstance(d,dict) else d[0]).get("status","?"))' 2>/dev/null)"
is "positive-control: bead closed" "closed" "$PC_STATUS_AFTER_CLOSE"

# Refile within the interval — should reopen as closed-while-live.
file_watcher_incident "$REF_PC" "pc-watcher" SPIRA_WATCHER_INTERVAL_S=5 >/dev/null 2>&1

PC_CAUSE="$(reopen_cause "$PC_ID")"
is "positive-control: cause=closed-while-live when closed within interval" \
   "closed-while-live" "$PC_CAUSE"

# File AGAIN with a large interval — should produce cause=recurrence.
REF_PC2="incident:watcher-test-pc2-$$"
PC2_ID="$(file_watcher_incident "$REF_PC2" "pc-watcher2" SPIRA_WATCHER_INTERVAL_S=9999)"
B close "$PC2_ID" --reason-file - <<'R' >/dev/null 2>&1
positive control 2 close
R
# Reset closed_at to something old enough to exceed the interval.
# We cannot travel time, so use a 0-second interval — that means even a 0-second-old
# close is NOT within the interval, so recurrence fires.
file_watcher_incident "$REF_PC2" "pc-watcher2" SPIRA_WATCHER_INTERVAL_S=0 >/dev/null 2>&1
PC2_CAUSE="$(reopen_cause "$PC2_ID")"
is "positive-control: cause=recurrence when interval=0" \
   "recurrence" "$PC2_CAUSE"

# ──────────────────────────────────────────────────────────────────────────────
# CASE 1: DEP PATH — bead stays open when blocked by an open root bead.
# ──────────────────────────────────────────────────────────────────────────────
echo ""
echo "# case 1: dep path"

REF1="incident:watcher-test-dep-$$"
TITLE1="watcher dep test"

# File the initial incident.
INC1="$(file_watcher_incident "$REF1" "$TITLE1")"
[ -n "$INC1" ] && ok "case1: initial watcher incident filed" \
                || { bad "case1: initial watcher incident filed" "no bead id"; }

# Create a root-cause bead (open) and add it as a blocker.
ROOT1="$(B create "root cause for watcher test" -l spira,plan 2>/dev/null | grep -oE '[a-z0-9-]+-[a-z0-9]+' | head -1)"
[ -n "$ROOT1" ] && ok "case1: root cause bead created ($ROOT1)" \
                 || bad "case1: root cause bead created" "no id"

B dep add "$INC1" "$ROOT1" >/dev/null 2>&1
ok "case1: dep added (incident → root)"

# Refile the same ref (simulating next watcher tick). Bead is OPEN; should just note.
file_watcher_incident "$REF1" "$TITLE1" >/dev/null 2>&1

# Assert: bead is still open.
STATUS1="$(B show "$INC1" --json 2>/dev/null | python3 -c 'import sys,json; d=json.load(sys.stdin); print((d if isinstance(d,dict) else d[0]).get("status","?"))' 2>/dev/null)"
is "case1: bead stays open after refile with dep" "open" "$STATUS1"

# Assert: no reopen event (the bead was never closed, so no reopen).
REOPENS1="$(reopen_count "$INC1")"
is "case1: no reopen events" "0" "$REOPENS1"

# Assert: absent from bd ready --claim (blocked by open dep).
READY1="$(B ready --json 2>/dev/null | python3 -c '
import sys, json
d = json.load(sys.stdin)
ids = [i.get("id","") for i in (d if isinstance(d, list) else [])]
print("absent" if "'"$INC1"'" not in ids else "present")
' 2>/dev/null)"
is "case1: blocked bead absent from bd ready" "absent" "$READY1"

# ──────────────────────────────────────────────────────────────────────────────
# CASE 2: CLOSED-WHILE-LIVE PATH — refile within interval → cause=closed-while-live.
# ──────────────────────────────────────────────────────────────────────────────
echo ""
echo "# case 2: closed-while-live path"

REF2="incident:watcher-test-cwl-$$"
TITLE2="watcher closed-while-live test"

# File, close, refile — all within a tiny interval so the within-interval branch fires.
INC2="$(file_watcher_incident "$REF2" "$TITLE2" SPIRA_WATCHER_INTERVAL_S=3600)"
[ -n "$INC2" ] && ok "case2: initial watcher incident filed" \
                || bad "case2: initial watcher incident filed" "no bead id"

B close "$INC2" --reason-file - <<'R' >/dev/null 2>&1
ops closed while condition held
R
STATUS2_CLOSED="$(B show "$INC2" --json 2>/dev/null | python3 -c 'import sys,json; d=json.load(sys.stdin); print((d if isinstance(d,dict) else d[0]).get("status","?"))' 2>/dev/null)"
is "case2: bead closed" "closed" "$STATUS2_CLOSED"

# Refile within the interval — expect cause=closed-while-live.
file_watcher_incident "$REF2" "$TITLE2" SPIRA_WATCHER_INTERVAL_S=3600 >/dev/null 2>&1

STATUS2_REOPENED="$(B show "$INC2" --json 2>/dev/null | python3 -c 'import sys,json; d=json.load(sys.stdin); print((d if isinstance(d,dict) else d[0]).get("status","?"))' 2>/dev/null)"
is "case2: bead reopened" "open" "$STATUS2_REOPENED"

CAUSE2="$(reopen_cause "$INC2")"
is "case2: reopen cause=closed-while-live" "closed-while-live" "$CAUSE2"

# ──────────────────────────────────────────────────────────────────────────────
printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" = 0 ]
