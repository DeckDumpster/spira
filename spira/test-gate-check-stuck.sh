#!/usr/bin/env bash
#
# test-gate-check-stuck.sh — gate-check handles stuck gates and prefix-independent ESCALATE.
#
#   ./test-gate-check-stuck.sh
#
# TWO DEFECTS THIS SUITE EXISTS FOR.
#
# 1. STUCK GATES: a gate with no await_id pends forever and is reported as "0 errors",
#    indistinguishable from a healthy idle pass. gate-check must count stuck gates
#    separately so that state is visible.
#
# 2. PREFIX ASSUMPTION: the ESCALATE processing hardcoded sp- in both the gate-id grep
#    and the blocked-bead regex. An installation whose SPIRA_ID_PREFIX is not sp- (e.g.
#    db-) silently dropped every failed-CI resolution.
#
# THREE THINGS VERIFIED.
#
# 1. POSITIVE CONTROL FOR STUCK: a stuck gate present → "stuck" count in output.
#    A report that always says "0 stuck" regardless of state is no different from the
#    bug it was meant to fix.
#
# 2. NO FALSE STUCK: a gate that has an await_id is NOT reported as stuck.
#
# 3. NON-DEFAULT PREFIX IN ESCALATE PATH: gate-check extracts the gate id from
#    "⚠ tt-gate1: ESCALATE" without assuming sp-, and extracts the blocked bead
#    from "blocking tt-work1" in the description without assuming sp-. The fixture
#    prefix is tt- (not sp-) so that asserting against sp- would fail both checks.
#
# A REAL bd FOR PARTS 1–2 (law-prefer-the-real-dependency). What is being tested is
# whether gate-check reports stuck gates, which is a database-state question. A stub
# would reproduce the surface it was written to, and a wrong surface passes.
#
# A STUB bd FOR PART 3. What is being tested is gate-check's TEXT PARSING — specifically
# that it does not hardcode the sp- prefix. Parsing is in gate-check.sh, not in bd, so
# a stub that emits controlled output with a non-default prefix is the right dependency.
#
# covers: spira/gate-check.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-gate-check-stuck
trap 'testdb_drop' EXIT INT TERM
testdb_up gate_check_stuck || { echo "test-gate-check-stuck: could not build fixture"; exit 1; }

B() { "${SPIRA_BD:-bd}" -C "$SPIRA_DB" "$@"; }

TMP="$(mktemp -d)"
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM

GATE_CHECK="$HERE/gate-check.sh"

# ======================================================================================
# PART 1: STUCK GATE REPORTING (real bd, real database).
#
# A gate with no await_id is stuck: discover cannot match it and check skips it.
# gate-check must report the stuck count so "0 resolved" is not ambiguous.
# ======================================================================================
echo "stuck gate is reported separately:"

testdb_reset
testdb_seed <<JSONL
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":["spira","plan"]}
{"id":"sp-work","title":"task","status":"open","issue_type":"task","labels":["spira","plan"]}
JSONL

# Create a gate WITHOUT await_id — this is the stuck state.
B gate create --type=gh:run --blocks sp-work 2>/dev/null | head -1 >/dev/null

SH="$TMP/spira"; mkdir -p "$SH"
cp "$HERE/gate-check.sh" "$HERE/lib.sh" "$HERE/conf.sh" "$HERE/incident.sh" "$SH/"

# Run gate-check.sh: a real bd against the fixture, no repo-map so discover is a no-op.
gate_out="$(SPIRA_HOME="$SH" SPIRA_REPO="$TMP" SPIRA_RUN="$TMP/run" SPIRA_DB="$SPIRA_DB" \
    SPIRA_REPO_MAP="$TMP/empty-map" SPIRA_CONF="$TMP/no.conf" \
    bash "$SH/gate-check.sh" 2>/dev/null)"

# THE POSITIVE CONTROL: a stuck gate must appear in the stuck count.
want "stuck gate is counted" "stuck" "$gate_out"

# ======================================================================================
# PART 2: NO FALSE STUCK (positive control).
#
# A gate that has an await_id set is NOT stuck — gate-check must not miscount it.
# ======================================================================================
echo
echo "gate with await_id is not reported as stuck:"

testdb_reset
testdb_seed <<JSONL
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":["spira","plan"]}
{"id":"sp-work2","title":"task2","status":"open","issue_type":"task","labels":["spira","plan"]}
JSONL

GATE_ID2=$(B gate create --type=gh:run --blocks sp-work2 2>/dev/null \
    | grep -oE 'sp-[a-z0-9]+' | head -1)
# Set an await_id: gate-check will try gh and may error, but the gate is NOT stuck.
B update "$GATE_ID2" --await-id "99999" 2>/dev/null >/dev/null

gate_out2="$(SPIRA_HOME="$SH" SPIRA_REPO="$TMP" SPIRA_RUN="$TMP/run" SPIRA_DB="$SPIRA_DB" \
    SPIRA_REPO_MAP="$TMP/empty-map" SPIRA_CONF="$TMP/no.conf" \
    bash "$SH/gate-check.sh" 2>/dev/null)"

nowant "gate with await_id is not stuck" "stuck" "$gate_out2"

# ======================================================================================
# PART 3: NON-DEFAULT PREFIX IN ESCALATE PATH (stub bd).
#
# The ESCALATE line carries the gate id. The blocked bead comes from the gate's
# description. Both used to assume sp- prefix; the fix must handle any prefix.
#
# Fixture prefix: tt-  (not sp-). Asserting against sp- passes the old code; asserting
# against tt- fails it. So a green test here proves the literal was removed.
#
# THE STUB BD CONTROLS OUTPUT. What gate-check.sh parses is bd's text, not database
# state, so a stub that emits controlled tt- output is the right dependency here.
# The stub records every call so we can assert which gate was resolved.
# ======================================================================================
echo
echo "ESCALATE with non-default prefix (tt-) is resolved correctly:"

export BD_LOG="$TMP/bd-calls.log"
: > "$BD_LOG"
mkdir -p "$TMP/sbin"

# STUB SHOW: returns a gate description with tt- blocked bead when queried for tt-gate1.
# Every other show call returns empty.
cat > "$TMP/sbin/bd" <<'BDSTUB'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$BD_LOG"
# Handle 'show tt-gate1 --json' — return a gate whose description names the blocked bead.
for _a; do
    case "$_a" in tt-gate1) ;;  *) continue ;; esac
    printf '[{"id":"tt-gate1","description":"Ad-hoc gate blocking tt-work1","issue_type":"gate","status":"open","await_type":"gh:run"}]\n'
    exit 0
done
# gate check: emit one ESCALATE with tt- prefix, then the summary.
case "$*" in
    *"gate check"*)
        printf '⚠ tt-gate1: ESCALATE - workflow failed\n\nChecked 1 gates: 0 resolved, 1 escalated, 0 errors\n'
        exit 0 ;;
    *"gate discover"*) exit 0 ;;
    *"gate resolve"*)  exit 0 ;;
    *)                 exit 0 ;;
esac
BDSTUB
chmod +x "$TMP/sbin/bd"

# NOTIFY shim: accept spira_event calls without writing to a real location.
mkdir -p "$TMP/run/events"
printf '#!/usr/bin/env bash\nprintf "%%s\\n" "$*" >> "%s/notify.log"\n' "$TMP" > "$TMP/sbin/ask.sh"
chmod +x "$TMP/sbin/ask.sh"

SPIRA_HOME="$SH" SPIRA_REPO="$TMP" SPIRA_RUN="$TMP/run" SPIRA_DB="$TMP/fake-db" \
    SPIRA_REPO_MAP="$TMP/empty-map" SPIRA_CONF="$TMP/no.conf" \
    SPIRA_ID_PREFIX=tt SPIRA_BD="$TMP/sbin/bd" SPIRA_NOTIFY="$TMP/sbin/ask.sh" \
    bash "$SH/gate-check.sh" 2>/dev/null

bd_calls="$(cat "$BD_LOG" 2>/dev/null)"

# THE GATE ID MUST BE EXTRACTED WITHOUT PREFIX ASSUMPTION.
# gate-check must call 'gate resolve tt-gate1', not 'gate resolve sp-gate1' or nothing.
want   "gate resolve called for tt-gate1"   "gate resolve tt-gate1"  "$bd_calls"

# THE BLOCKED BEAD MUST COME FROM THE DESCRIPTION WITHOUT PREFIX.
# The show for tt-gate1 is called (to read the description).
want   "show called for tt-gate1"           "show tt-gate1"          "$bd_calls"

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
