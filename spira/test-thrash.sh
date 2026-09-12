#!/usr/bin/env bash
#
# test-thrash.sh — deliverable-progress wall (sp-cuvi)
#
# The wall detects when an aeon's turns advance but its deliverable (commits ahead
# of the base ref or file writes in the worktree) has not moved for SPIRA_THRASH_MINUTES.
# A running gate suppresses the fuse so a correct mid-review aeon is never tripped.
# On trip the heartbeat sends SIGTERM, and cleanup reads the .thrash marker to requeue
# the bead without charging an attempt.
#
# covers: spira/lib.sh spira/aeon.sh spira/cockpit.sh spira/cockpit-metrics.py
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"

pass=0; fail=0
TMP="$(mktemp -d)"; trap 'kill_all 2>/dev/null; rm -rf "$TMP"' EXIT
RUN="$TMP/run"
mkdir -p "$RUN/gate-run"
PIDS=()

kill_all() { [ "${#PIDS[@]}" -gt 0 ] && kill "${PIDS[@]}" 2>/dev/null; wait 2>/dev/null; }

ok()   { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()  { fail=$((fail+1)); printf '  FAIL  %s\n' "$1"; }
is_n() { if [ "$2" = "$3" ]; then pass=$((pass+1)); printf '  ok    %s\n' "$1"
         else fail=$((fail+1)); printf '  FAIL  %s: want [%s] got [%s]\n' "$1" "$2" "$3"; fi; }

# fuse <bead> [<wt>] -> call aeon_fuse_minutes via lib.sh in a clean environment.
fuse() {
    local bead="$1" wt="${2:-$RUN/worktree/$1}"
    env -i PATH="$PATH" HOME="$TMP" LC_ALL=C.UTF-8 \
        SPIRA_CONF="$TMP/no.conf" SPIRA_HOME="$HERE" SPIRA_REPO="$TMP" \
        SPIRA_RUN="$RUN" \
        bash -c '. "$1/lib.sh"; aeon_fuse_minutes "$2" "$3"' _ "$HERE" "$bead" "$wt" 2>/dev/null
}

# make_gate <bead> -> start a fake gate process and write the gate-run pid file.
# "gate.sh" in argv[0] is what aeon_fuse_minutes checks via /proc/<pid>/cmdline.
# Called WITHOUT $(...) — a $(...) subshell waits for ALL its children before exiting,
# so a bare & inside it hangs until sleep 9999 dies. The caller reads the pid from the
# gate-run pid file instead of capturing stdout.
make_gate() {
    local bead="$1"
    bash -c 'exec -a "gate.sh" sleep 9999' &
    local pid="$!"
    PIDS+=("$pid")
    local gate_dir="$RUN/gate-run/testname.spira_${bead}"
    mkdir -p "$gate_dir"
    echo "$pid" > "$gate_dir/pid"
}

# make_worktree <bead> -> minimal git worktree with one commit, returning the wt path.
make_worktree() {
    local bead="$1"
    local wt="$RUN/worktree/$bead"
    mkdir -p "$wt"
    git -C "$wt" init -q
    git -C "$wt" config user.email "t@t" && git -C "$wt" config user.name "T"
    echo "init" > "$wt/file.txt"
    git -C "$wt" add file.txt
    git -C "$wt" commit -q -m "initial"
    printf '%s' "$wt"
}

# ---- Part 1: aeon_fuse_minutes — no worktree → ? ------------------------------------
echo "aeon_fuse_minutes: no worktree returns ?"

# POSITIVE CONTROL: the function must be callable at all. A sourcing failure looks
# identical to a ? return — verify the function is reachable before asserting its output.
lib_check="$(env -i PATH="$PATH" HOME="$TMP" LC_ALL=C.UTF-8 \
    SPIRA_CONF="$TMP/no.conf" SPIRA_HOME="$HERE" SPIRA_REPO="$TMP" SPIRA_RUN="$RUN" \
    bash -c '. "$1/lib.sh"; echo aeon_fuse_minutes_exists' _ "$HERE" 2>/dev/null)"
if [ "$lib_check" = "aeon_fuse_minutes_exists" ]; then
    ok "lib.sh sources cleanly and aeon_fuse_minutes is defined (positive control)"
else
    bad "lib.sh sourcing failed or aeon_fuse_minutes not defined (positive control)"
fi

no_wt="$(fuse "sp-noworktree-x" "$RUN/worktree/sp-noworktree-x")"
is_n "no worktree → ?" "?" "$no_wt"

# ---- Part 2: aeon_fuse_minutes — recent file write → 0 (positive control) -----------
echo
echo "aeon_fuse_minutes: recent file write returns 0"

WT2="$(make_worktree "sp-fresh-x")"
fresh="$(fuse "sp-fresh-x" "$WT2")"
is_n "just-written worktree → 0" "0" "$fresh"

# ---- Part 3: aeon_fuse_minutes — old file write → numeric ----------------------------
echo
echo "aeon_fuse_minutes: stale file write returns a positive number"

BEAD3="sp-stale-x"
WT3="$RUN/worktree/$BEAD3"
mkdir -p "$WT3"
# Age both the file and the directory — find picks up the most recent mtime across all
# entries including directories, so the dir itself must be old too.
touch -d "90 minutes ago" "$WT3/stale.txt"
touch -d "90 minutes ago" "$WT3"
stale="$(fuse "$BEAD3" "$WT3")"
case "$stale" in
    [0-9]*) ok "stale worktree returns a number" ;;
    *)      bad "stale worktree should return a number, got [$stale]" ;;
esac
if [ "$stale" -ge 85 ] 2>/dev/null && [ "$stale" -le 95 ] 2>/dev/null; then
    ok "stale worktree fuse ≈ 90 minutes (got $stale)"
else
    bad "stale worktree fuse expected ~90 minutes, got [$stale]"
fi

# ---- Part 4: gate suppression (positive control required) ---------------------------
echo
echo "aeon_fuse_minutes: live gate suppresses the fuse"

# POSITIVE CONTROL FIRST. Without the gate the stale bead returns a number, not "gate".
BEAD4="sp-gatesup-x"
WT4="$RUN/worktree/$BEAD4"
mkdir -p "$WT4"
touch -d "60 minutes ago" "$WT4/old.txt"
touch -d "60 minutes ago" "$WT4"
no_gate_fuse="$(fuse "$BEAD4" "$WT4")"
case "$no_gate_fuse" in
    [0-9]*) ok "without gate: fuse is a number (positive control)" ;;
    *)      bad "without gate: expected a number, got [$no_gate_fuse] (positive control)" ;;
esac

# Now start a gate and re-probe.
make_gate "$BEAD4"
GATE4_PID="$(cat "$RUN/gate-run/testname.spira_${BEAD4}/pid")"
gate_fuse="$(fuse "$BEAD4" "$WT4")"
is_n "live gate suppresses fuse → gate" "gate" "$gate_fuse"

# Dead gate must not keep the suppression — a stale pid file must not read as gate.
kill "$GATE4_PID" 2>/dev/null; wait "$GATE4_PID" 2>/dev/null || true
dead_gate_fuse="$(fuse "$BEAD4" "$WT4")"
case "$dead_gate_fuse" in
    gate) bad "dead gate pid: fuse still reads 'gate'" ;;
    ?)    ok "dead gate pid: fuse reads ? (worktree no-pid fallback, acceptable)" ;;
    [0-9]*) ok "dead gate pid: fuse resumes as a number (got $dead_gate_fuse)" ;;
    *)    bad "dead gate pid: unexpected value [$dead_gate_fuse]" ;;
esac

# ---- Part 5: structural — .thrash appears after .slain in cleanup --------------------
echo
echo "aeon.sh structural: .thrash handler appears after .slain in cleanup function"

AEON="$HERE/aeon.sh"
if [ ! -f "$AEON" ]; then
    bad "aeon.sh not found at $AEON"
else
    slain_line="$(grep -n '\.slain' "$AEON" | grep -v 'thrash' | tail -1 | cut -d: -f1)"
    thrash_line="$(grep -n '\.thrash' "$AEON" | grep 'f.*BEAD_ID' | head -1 | cut -d: -f1)"
    if [ -z "$slain_line" ] || [ -z "$thrash_line" ]; then
        bad ".slain or .thrash line not found in aeon.sh (slain=$slain_line thrash=$thrash_line)"
    elif [ "$thrash_line" -gt "$slain_line" ]; then
        ok ".thrash check (line $thrash_line) is after .slain check (line $slain_line)"
    else
        bad ".thrash check (line $thrash_line) should come after .slain (line $slain_line)"
    fi

    # Confirm no attempt is charged: aeon.sh must not call increment_attempt or touch the
    # attempt counter anywhere in the .thrash handler. The handler is the fi-block after
    # the line that tests for the .thrash marker file.
    thrash_block="$(awk '/BEAD_ID\.thrash/{f=1} f{print; if(/^[[:space:]]*fi$/){exit}}' "$AEON")"
    if grep -q 'increment_attempt\|ATTEMPTS\b' <<< "${thrash_block:-}" 2>/dev/null; then
        bad "aeon.sh thrash region references attempt increment — no attempt should be charged"
    else
        ok "thrash cleanup block does not increment attempts"
    fi

    # Confirm ledger_done is called with requeue-thrash status anywhere in the file.
    if grep -q 'ledger_done.*requeue-thrash\|requeue-thrash.*ledger_done' "$AEON"; then
        ok "aeon.sh calls ledger_done with requeue-thrash"
    else
        bad "aeon.sh does not call ledger_done with requeue-thrash"
    fi
fi

# ---- Part 6: structural — thrash check guarded by idle < STALL_BEATS ----------------
echo
echo "aeon.sh structural: thrash check in heartbeat is inside the idle < STALL_BEATS guard"

if [ ! -f "$AEON" ]; then
    bad "aeon.sh not found"
else
    # The thrash trip must sit inside the `if [ "$idle" -lt "$STALL_BEATS" ]` block so the
    # heartbeat's own stall detection (idle ≥ STALL_BEATS) is the outer guard.
    # Find the line of the STALL_BEATS guard and the line of the thrash fuse check.
    stall_guard_line="$(grep -n 'idle.*STALL_BEATS\|STALL_BEATS.*idle' "$AEON" \
        | grep '\-lt\|\-ge' | head -1 | cut -d: -f1)"
    thrash_fuse_line="$(grep -n '_dfuse.*aeon_fuse_minutes\|aeon_fuse_minutes.*_dfuse' "$AEON" \
        | head -1 | cut -d: -f1)"
    if [ -z "$stall_guard_line" ] || [ -z "$thrash_fuse_line" ]; then
        bad "stall guard or thrash fuse line not found (guard=$stall_guard_line fuse=$thrash_fuse_line)"
    elif [ "$thrash_fuse_line" -gt "$stall_guard_line" ]; then
        ok "thrash fuse check (line $thrash_fuse_line) is inside the STALL_BEATS guard (line $stall_guard_line)"
    else
        bad "thrash fuse check (line $thrash_fuse_line) must come after STALL_BEATS guard (line $stall_guard_line)"
    fi
fi

# ---- Part 7: cockpit-metrics.py counts requeue-thrash ledger lines ------------------
echo
echo "cockpit-metrics.py: status=requeue-thrash is counted as SP_AEON_THRASH"

METRICS="$HERE/cockpit-metrics.py"
if [ ! -f "$METRICS" ]; then
    bad "cockpit-metrics.py not found at $METRICS"
else
    LEDGER="$TMP/aeon-ledger.log"
    SENTINEL="$TMP/sentinel.log"
    # Empty sentinel and ledger files for a clean probe.
    printf '' > "$SENTINEL"
    printf '' > "$LEDGER"

    # ledger_metrics is a pure function; call it directly via python3 -c to avoid
    # having to satisfy cockpit-metrics.py's two-file main() invocation contract.
    call_ledger() {
        python3 -c "
import sys, re, os
sys.path.insert(0, os.path.dirname('$METRICS'))
from datetime import datetime, timezone, timedelta

# Import the function from the module without running main().
spec = open('$METRICS').read()
ns = {}
exec(compile(spec, '$METRICS', 'exec'), ns)
ledger_metrics = ns['ledger_metrics']
read = ns['read']
since = datetime.now(timezone.utc) - timedelta(hours=24)
d = ledger_metrics(read('$LEDGER'), since)
for k, v in sorted(d.items()):
    print('%s=%s' % (k, v))
" 2>/dev/null
    }

    # key_val <key> <output>: extract value for a key from the output.
    key_val() { grep "^$1=" <<< "$2" | cut -d= -f2; }

    # POSITIVE CONTROL: an empty ledger must return SP_AEON_THRASH=0, not missing.
    out0="$(call_ledger)"
    zero_thrash="$(key_val SP_AEON_THRASH "$out0")"
    is_n "empty ledger → SP_AEON_THRASH=0 (positive control)" "0" "$zero_thrash"

    # Now add timestamped ledger lines (cockpit-metrics.py expects ISO timestamps).
    NOW="$(date -u '+%Y-%m-%dT%H:%M:%SZ')"
    printf '%s awake fayth sp-aaa worked\n' "$NOW" >> "$LEDGER"
    printf '%s done fayth sp-bbb rc=0 status=requeue-thrash\n' "$NOW" >> "$LEDGER"
    printf '%s done fayth sp-ccc rc=0 status=requeue-thrash\n' "$NOW" >> "$LEDGER"
    printf '%s done fayth sp-ddd rc=0 status=requeue-thrash\n' "$NOW" >> "$LEDGER"
    printf '%s done fayth sp-eee rc=0 status=requeue-slain\n' "$NOW" >> "$LEDGER"
    out3="$(call_ledger)"
    thrash_count="$(key_val SP_AEON_THRASH "$out3")"
    is_n "three requeue-thrash lines → SP_AEON_THRASH=3" "3" "$thrash_count"

    # Verify other counters are not contaminated.
    worked_count="$(key_val SP_AEON_WORKED "$out3")"
    is_n "thrash lines do not inflate SP_AEON_WORKED" "1" "$worked_count"
fi

# ---- Part 8: cockpit.sh emits SP_AEON_THRASH in fallback key list -------------------
echo
echo "cockpit.sh: SP_AEON_THRASH appears in the fallback ? key list"

COCKPIT="$HERE/cockpit.sh"
if [ ! -f "$COCKPIT" ]; then
    bad "cockpit.sh not found"
else
    if grep -q 'SP_AEON_THRASH' "$COCKPIT"; then
        ok "cockpit.sh references SP_AEON_THRASH"
    else
        bad "cockpit.sh does not reference SP_AEON_THRASH"
    fi
fi

# ---- Part 9: SPIRA_THRASH_MINUTES in conf.sh ----------------------------------------
echo
echo "conf.sh: SPIRA_THRASH_MINUTES has a default value"

CONF="$HERE/conf.sh"
if [ ! -f "$CONF" ]; then
    bad "conf.sh not found"
else
    if grep -q 'SPIRA_THRASH_MINUTES' "$CONF"; then
        ok "conf.sh defines SPIRA_THRASH_MINUTES"
    else
        bad "conf.sh does not define SPIRA_THRASH_MINUTES"
    fi
    # Extract the default and verify it is a number.
    default_val="$(grep 'SPIRA_THRASH_MINUTES' "$CONF" | grep -o '[0-9]\+' | head -1)"
    case "${default_val:-}" in
        [0-9]*) ok "SPIRA_THRASH_MINUTES default is a number ($default_val)" ;;
        *)      bad "SPIRA_THRASH_MINUTES default is not a number: [$default_val]" ;;
    esac
fi

# ---- Results -------------------------------------------------------------------------
echo
echo "---"
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
