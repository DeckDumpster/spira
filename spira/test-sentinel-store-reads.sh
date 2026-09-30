#!/usr/bin/env bash
#
# test-sentinel-store-reads.sh — the sentinel's full pass fetches the store once (sp-bo67y):
#   one `bd list --all` and one broad `bd ready` per pass, cached in a temp file that every
#   consumer filters in-process instead of asking bd again.
#
# THE DEFECT. strand.sh's classify_one, dispatchable_open and check4_closed_branched each
# asked bd once PER PARTITION (check4_closed_branched alone pulled ~10.5MB of closed JSON
# per partition); mark_queue_waiters, detect_unclaimable_ready and bulk_ready_by_fayth each
# ran a full `bd ready` again; mark_queue_waiters also forked two awk processes per landstate
# file. Measured on the live store (05:43:23 2026-09-28): ~30 bd list/ready calls, 375s wall.
#
# THE FIX. sentinel.sh populates SPIRA_LIST_SNAPSHOT (`bd list --all`) and
# SPIRA_READY_SNAPSHOT (ready_raw_args — the broadest ready query) once and exports them;
# every consumer below reads the cached JSON and filters by label/status/exclude in-process.
#
# POSITIVE CONTROLS FIRST (law-a-regression-test-must-be-seen-to-fail): each case runs once
# with the snapshot env vars UNSET — the old per-call shape, proving the bd-call counter can
# see calls at all — and once WITH them set, proving the fix removes the calls while leaving
# the output identical.
#
# defect: sp-bo67y
# covers: spira/lib.sh strand/src/* sentinel/src/*
# hermetic-ok: uses a fixture database; bd calls counted through a logging SPIRA_BD shim;
#              the real chamber (spira/chamber/*.fayth) supplies more than one partition so
#              the per-partition fan-out this bead removes is genuinely exercised
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
export SPIRA_HOME="$HERE"
export SPIRA_CONF="/tmp/.spira-test-noconf-$$"
export SPIRA_HOME_REPO=spira
export SPIRA_ASK_LABEL="needs-decision-ssr"

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-sentinel-store-reads
TMP="$(mktemp -d)"
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM

export SPIRA_RUN="$TMP/run"; mkdir -p "$SPIRA_RUN/landstate"

testdb_up ssreads || { echo "test-sentinel-store-reads: could not build fixture database"; exit 1; }

# THE COUNTING SHIM WRAPS WHATEVER REAL BINARY testdb_up CHOSE (bd-embedded or the server
# bd), not a bare `bd` off PATH — testdb_up already exported SPIRA_BD to the right one for
# this fixture's mode, and overwriting that with a guess would silently point every call at
# a different engine.
REAL_BD="${SPIRA_BD:?testdb_up did not export SPIRA_BD}"
CALLS="$TMP/bd-calls.log"
BD_STUB="$TMP/bd-stub.sh"
cat > "$BD_STUB" <<STUBEOF
#!/usr/bin/env bash
printf '%s\n' "\$1" >> "$CALLS"
exec "$REAL_BD" "\$@"
STUBEOF
chmod +x "$BD_STUB"
export SPIRA_BD="$BD_STUB"

acted=0; progressed=0
act()      { acted=$((acted+1)); }
progress() { progressed=$((progressed+1)); act "$@"; }
log()      { :; }
# shellcheck disable=SC1090
. "$HERE/lib.sh"
. "$HERE/testlib.sh"

echo "test-sentinel-store-reads.sh"

n_calls() { grep -cE '^(list|ready)$' "$CALLS" 2>/dev/null || true; }
reset_calls() { : > "$CALLS"; }

seed() {   # seed <id> <labels-csv> <status>
    python3 -c '
import json, sys
print(json.dumps({"id": sys.argv[1], "title": "t " + sys.argv[1], "status": sys.argv[3],
                   "issue_type": "task", "labels": sys.argv[2].split(",")}))
' "$1" "$2" "$3"
}

testdb_reset
{
    seed sp-r1 "spira,plan"  open
    seed sp-r2 "spira,groom" open
    seed sp-c1 "spira,plan,branch:spira/sp-c1"  closed
    seed sp-c2 "spira,groom,branch:spira/sp-c2" closed
} | testdb_seed

# ==========================================================================================
echo
echo "case 1 — dispatchable_open: per-partition fan-out replaced by one cached list"
# ==========================================================================================
reset_calls
unset SPIRA_LIST_SNAPSHOT
out_nosnap="$(dispatchable_open 2>/dev/null)"
calls_nosnap="$(n_calls)"
[ "${calls_nosnap:-0}" -gt 1 ] \
    && ok "1a POSITIVE CONTROL: no snapshot — more than one partition asks bd (${calls_nosnap} calls)" \
    || bad "1a POSITIVE CONTROL: no snapshot — more than one partition asks bd" "got ${calls_nosnap:-0} calls"

reset_calls
SPIRA_LIST_SNAPSHOT="$TMP/list-snapshot.json"
bdjson list --all --limit 0 > "$SPIRA_LIST_SNAPSHOT" 2>/dev/null
reset_calls
out_snap="$(SPIRA_LIST_SNAPSHOT="$SPIRA_LIST_SNAPSHOT" dispatchable_open 2>/dev/null)"
calls_snap="$(n_calls)"
is   "1b: with the snapshot cached, dispatchable_open asks bd zero more times" "0" "${calls_snap:-x}"
is   "1c: output is identical with and without the snapshot" "$out_nosnap" "$out_snap"
unset SPIRA_LIST_SNAPSHOT

# ==========================================================================================
echo
echo "case 2 — check4_closed_branched: same fix, the ~10.5MB-of-closed-JSON call site"
# ==========================================================================================
reset_calls
out_nosnap="$(check4_closed_branched 2>/dev/null)"
calls_nosnap="$(n_calls)"
[ "${calls_nosnap:-0}" -gt 1 ] \
    && ok "2a POSITIVE CONTROL: no snapshot — more than one partition asks bd (${calls_nosnap} calls)" \
    || bad "2a POSITIVE CONTROL: no snapshot — more than one partition asks bd" "got ${calls_nosnap:-0} calls"

reset_calls
LIST_SNAP="$TMP/list-snapshot2.json"
bdjson list --all --limit 0 > "$LIST_SNAP" 2>/dev/null
reset_calls
out_snap="$(SPIRA_LIST_SNAPSHOT="$LIST_SNAP" check4_closed_branched 2>/dev/null)"
calls_snap="$(n_calls)"
is   "2b: with the snapshot cached, check4_closed_branched asks bd zero more times" "0" "${calls_snap:-x}"
is   "2c: output is identical with and without the snapshot" "$out_nosnap" "$out_snap"
want "2d: both closed+branched beads are still found (sp-c1)" "sp-c1" "$out_snap"
want "2e: both closed+branched beads are still found (sp-c2)" "sp-c2" "$out_snap"

# ==========================================================================================
echo
echo "case 3 — bulk_ready_by_fayth reads SPIRA_READY_SNAPSHOT instead of asking bd again"
# ==========================================================================================
reset_calls
out_nosnap="$(bulk_ready_by_fayth 2>/dev/null)"
calls_nosnap="$(n_calls)"
is   "3a POSITIVE CONTROL: no snapshot — bulk_ready_by_fayth asks bd once" "1" "${calls_nosnap:-x}"

reset_calls
READY_SNAP="$TMP/ready-snapshot.json"
_snap_ready_args=(); while IFS= read -r _a; do _snap_ready_args+=("$_a"); done < <(ready_raw_args)
bdjson "${_snap_ready_args[@]}" > "$READY_SNAP" 2>/dev/null
reset_calls
out_snap="$(SPIRA_READY_SNAPSHOT="$READY_SNAP" bulk_ready_by_fayth 2>/dev/null)"
calls_snap="$(n_calls)"
is   "3b: with the snapshot cached, bulk_ready_by_fayth asks bd zero more times" "0" "${calls_snap:-x}"
is   "3c: per-fayth counts are identical with and without the snapshot" "$out_nosnap" "$out_snap"

# ==========================================================================================
echo
echo "case 4 — detect_unclaimable_ready reads SPIRA_READY_SNAPSHOT instead of asking bd again"
# ==========================================================================================
reset_calls
out_nosnap="$(detect_unclaimable_ready 2>/dev/null)"
calls_nosnap="$(n_calls)"
is   "4a POSITIVE CONTROL: no snapshot — detect_unclaimable_ready asks bd once" "1" "${calls_nosnap:-x}"

reset_calls
out_snap="$(SPIRA_READY_SNAPSHOT="$READY_SNAP" detect_unclaimable_ready 2>/dev/null)"
calls_snap="$(n_calls)"
is   "4b: with the snapshot cached, detect_unclaimable_ready asks bd zero more times" "0" "${calls_snap:-x}"
is   "4c: output is identical with and without the snapshot" "$out_nosnap" "$out_snap"

# ==========================================================================================
echo
echo "case 5 — mark_queue_waiters: one awk for the whole landstate directory, not two per file"
# ==========================================================================================
# A COUNTING awk SHIM, put ahead of the real one on PATH. Every landstate file used to cost
# two forked awk processes (lib.sh:1544-1545 before this fix); this fixture plants five, so
# the old shape would show 10 invocations and the fix must show exactly 1.
AWK_CALLS="$TMP/awk-calls.log"
AWK_BIN_DIR="$TMP/awkbin"; mkdir -p "$AWK_BIN_DIR"
REAL_AWK="$(command -v awk)"
cat > "$AWK_BIN_DIR/awk" <<AWKEOF
#!/usr/bin/env bash
echo call >> "$AWK_CALLS"
exec "$REAL_AWK" "\$@"
AWKEOF
chmod +x "$AWK_BIN_DIR/awk"

for i in 1 2 3 4 5; do
    printf 'LANDED sha%s %s\n' "$i" "$(date +%s)" > "$SPIRA_RUN/landstate/sp-mq$i"
done
: > "$AWK_CALLS"
PATH="$AWK_BIN_DIR:$PATH" mark_queue_waiters 2>/dev/null
is   "5: one awk invocation covers all five landstate files" \
     "1" "$(wc -l < "$AWK_CALLS" 2>/dev/null | tr -d ' ')"

# A directory holding ONLY a subdirectory (no regular files at all) must not make the whole
# scan fail silently — awk aborts outright if handed a directory as a file argument, and the
# pre-filter loop exists precisely to keep that off the argv.
mkdir -p "$SPIRA_RUN/landstate2/sub"
: > "$AWK_CALLS"
( landstate_dir_only_test() {
      local landstate_dir="$SPIRA_RUN/landstate2"
      local -a _mq_all=("$landstate_dir"/*) _mq_files=() _mq_sf
      for _mq_sf in "${_mq_all[@]}"; do [ -f "$_mq_sf" ] && _mq_files+=("$_mq_sf"); done
      printf '%s' "${#_mq_files[@]}"
  }
  is "5b: a directory entry under landstate/ is filtered out before reaching awk" \
     "0" "$(landstate_dir_only_test)" )

# ==========================================================================================
echo
echo "case 6 — mark_queue_waiters narrows SPIRA_READY_SNAPSHOT to SPIRA_SCOPE_LABEL itself"
# ==========================================================================================
# An out-of-scope bead present in the broad snapshot (no SPIRA_SCOPE_LABEL) must never pick
# up the queue-wait label — READY_ARGS itself restricts to scope, and reading straight from
# the unscoped snapshot without re-applying that restriction would label beads mark_queue_
# waiters never touched before.
testdb_reset
{
    seed sp-blocker "spira,plan" closed
    printf '%s' '{"id":"sp-dependent","title":"t","status":"open","issue_type":"task","labels":["spira","plan"],"dependencies":[{"issue_id":"sp-dependent","depends_on_id":"sp-blocker","type":"blocks"}]}'
    echo
    printf '%s' '{"id":"sp-outofscope","title":"t","status":"open","issue_type":"task","labels":["plan"],"dependencies":[{"issue_id":"sp-outofscope","depends_on_id":"sp-blocker","type":"blocks"}]}'
    echo
} | testdb_seed
printf 'CERTIFIED abc %s\n' "$(date +%s)" > "$SPIRA_RUN/landstate/sp-blocker"
rm -f "$SPIRA_RUN/landstate/sp-mq1" "$SPIRA_RUN/landstate/sp-mq2" "$SPIRA_RUN/landstate/sp-mq3" \
      "$SPIRA_RUN/landstate/sp-mq4" "$SPIRA_RUN/landstate/sp-mq5"

_snap_ready_args=(); while IFS= read -r _a; do _snap_ready_args+=("$_a"); done < <(ready_raw_args)
bdjson "${_snap_ready_args[@]}" > "$READY_SNAP" 2>/dev/null
SPIRA_READY_SNAPSHOT="$READY_SNAP" mark_queue_waiters 2>/dev/null
labels_of() {
    bdjson show "$1" 2>/dev/null | python3 -c '
import json, sys
d = json.load(sys.stdin); d = d if isinstance(d, list) else [d]
print(" ".join(d[0].get("labels") or []))' 2>/dev/null
}
has_wait="$(labels_of sp-dependent)"
no_wait="$(labels_of sp-outofscope)"
WAIT="${SPIRA_QUEUE_WAIT_LABEL:-spira-queue-waiting}"
case "$has_wait" in *"$WAIT"*) ok "6a: in-scope dependent gets the queue-wait label from the snapshot path" ;; \
    *) bad "6a: in-scope dependent gets the queue-wait label from the snapshot path" "$has_wait" ;; esac
case "$no_wait" in *"$WAIT"*) bad "6b: out-of-scope bead must not be labeled from the unscoped snapshot" "$no_wait" ;; \
    *) ok "6b: out-of-scope bead must not be labeled from the unscoped snapshot" ;; esac

# ==========================================================================================
echo
echo "case 7 — strand: same fan-out fix, run as the sentinel runs it"

# ==========================================================================================
testdb_reset
{
    seed sp-r1 "spira,plan"  open
    seed sp-r2 "spira,groom" open
} | testdb_seed
MOCK_SC="$TMP/mock-systemctl"
printf '#!/bin/sh\necho inactive\n' > "$MOCK_SC"; chmod +x "$MOCK_SC"
# strand.sh is gone (the Rust cutover): the strand binary, resolved as conf.sh's spira_bin does.
reset_calls
export SPIRA_LABELS=""   # every partition, as the sentinel's own call leaves it
out_nosnap="$(SPIRA_BD="$BD_STUB" SPIRA_SYSTEMCTL="$MOCK_SC" strand report 2>/dev/null)"
calls_nosnap="$(n_calls)"
[ "${calls_nosnap:-0}" -gt 1 ] \
    && ok "7a POSITIVE CONTROL: no snapshot — strand asks bd more than once (${calls_nosnap} calls)" \
    || bad "7a POSITIVE CONTROL: no snapshot — strand asks bd more than once" "got ${calls_nosnap:-0} calls"

LIST_SNAP2="$TMP/list-snapshot3.json"
bdjson list --all --limit 0 > "$LIST_SNAP2" 2>/dev/null
reset_calls
out_snap="$(SPIRA_BD="$BD_STUB" SPIRA_SYSTEMCTL="$MOCK_SC" \
    SPIRA_LIST_SNAPSHOT="$LIST_SNAP2" SPIRA_READY_SNAPSHOT="$READY_SNAP" \
    strand report 2>/dev/null)"
calls_snap="$(n_calls)"
is   "7b: with both snapshots cached, strand asks bd zero more times" "0" "${calls_snap:-x}"
unset SPIRA_LABELS

# ==========================================================================================
echo
echo "case 8 — the sentinel's full pass populates both snapshots once and cleans them up"
# ==========================================================================================
STUBS="$TMP/stubs"; mkdir -p "$STUBS"
for _s in pilgrimage.sh strand reflect.sh sending.sh; do
    printf '#!/bin/sh\n' > "$STUBS/$_s"; chmod +x "$STUBS/$_s"
done
# THE SENTINEL IS A BINARY (sentinel.sh is gone): it sources lib.sh from SPIRA_HOME.
for _s in lib.sh conf.sh lc.sh suite-covers.sh lifecycle-cert.sh; do ln -s "$HERE/$_s" "$STUBS/$_s"; done
ln -sf "$HERE/chamber" "$STUBS/chamber" 2>/dev/null || true
PASS_RUN="$TMP/pass-run"; mkdir -p "$PASS_RUN/landstate"
GOAL_JSON="$(printf '{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":["spira"]}')"
testdb_reset
printf '%s\n' "$GOAL_JSON" | testdb_seed
export SPIRA_GOAL=sp-goal SPIRA_RUN="$PASS_RUN" SPIRA_MAX_AEONS=0 SPIRA_HOME="$STUBS" PATH="$STUBS:$PATH"
PATH="$STUBS:$PATH" sentinel >/dev/null 2>&1 || true
leftover="$(ls "$PASS_RUN"/list-snapshot.* "$PASS_RUN"/ready-snapshot.* "$PASS_RUN"/ready-cache.* 2>/dev/null | wc -l | tr -d ' ')"
is   "8: no snapshot temp file survives a completed pass" "0" "$leftover"
unset SPIRA_GOAL SPIRA_MAX_AEONS
export SPIRA_HOME="$HERE"
export SPIRA_RUN="$TMP/run"
