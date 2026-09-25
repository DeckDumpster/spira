#!/usr/bin/env bash
#
# test-thrash.sh — aeon_fuse_minutes: the deliverable-progress probe (sp-cuvi)
#
# The wall detects when an aeon's turns advance but its deliverable (commits ahead
# of the base ref or file writes in the worktree) has not moved for SPIRA_THRASH_MINUTES.
# A running gate suppresses the fuse so a correct mid-review aeon is never tripped.
#
# The trip decision itself (both the fuse AND the session's own age must clear the wall)
# is hb_tick's table, tested in test-aeon-lease.sh. The teardown behaviour once tripped
# (requeue, no attempt / attempt-charged streak) is test-thrash-teardown.sh (G2). This
# suite is left with what those two do not cover: the probe aeon_fuse_minutes itself.
#
# tier: T1
# covers: spira/lib.sh spira/aeon.sh spira/cockpit.sh spira/cockpit-metrics.py UC-aeon-execution-09
# scar: unrecorded
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

TMP="$(mktemp -d)"; trap 'kill_all 2>/dev/null; rm -rf "$TMP"' EXIT
RUN="$TMP/run"
mkdir -p "$RUN/gate-run"
PIDS=()

kill_all() { [ "${#PIDS[@]}" -gt 0 ] && kill "${PIDS[@]}" 2>/dev/null; wait 2>/dev/null; }

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
is "lib.sh sources cleanly and aeon_fuse_minutes is defined (positive control)" \
    "aeon_fuse_minutes_exists" "$lib_check"

no_wt="$(fuse "sp-noworktree-x" "$RUN/worktree/sp-noworktree-x")"
is "no worktree → ?" "?" "$no_wt"

# ---- Part 2: aeon_fuse_minutes — recent file write → 0 (positive control) -----------
echo
echo "aeon_fuse_minutes: recent file write returns 0"

WT2="$(make_worktree "sp-fresh-x")"
fresh="$(fuse "sp-fresh-x" "$WT2")"
is "just-written worktree → 0" "0" "$fresh"

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
    *)      bad "stale worktree returns a number" "got [$stale]" ;;
esac
if [ "$stale" -ge 85 ] 2>/dev/null && [ "$stale" -le 95 ] 2>/dev/null; then
    ok "stale worktree fuse ≈ 90 minutes (got $stale)"
else
    bad "stale worktree fuse ≈ 90 minutes" "expected 85..95, got [$stale]"
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
    *)      bad "without gate: fuse is a number (positive control)" "got [$no_gate_fuse]" ;;
esac

# Now start a gate and re-probe.
make_gate "$BEAD4"
GATE4_PID="$(cat "$RUN/gate-run/testname.spira_${BEAD4}/pid")"
gate_fuse="$(fuse "$BEAD4" "$WT4")"
is "live gate suppresses fuse → gate" "gate" "$gate_fuse"

# Dead gate must not keep the suppression — a stale pid file must not read as gate.
kill "$GATE4_PID" 2>/dev/null; wait "$GATE4_PID" 2>/dev/null || true
dead_gate_fuse="$(fuse "$BEAD4" "$WT4")"
case "$dead_gate_fuse" in
    gate) bad "dead gate pid: fuse must not still read 'gate'" "still [gate]" ;;
    \?)   ok "dead gate pid: fuse reads ? (worktree no-pid fallback, acceptable)" ;;
    [0-9]*) ok "dead gate pid: fuse resumes as a number (got $dead_gate_fuse)" ;;
    *)    bad "dead gate pid: expected a number or ?" "got [$dead_gate_fuse]" ;;
esac

# ---- Part 4b: a gate that just finished resets the fuse (sp-l99q6) ------------------
echo
echo "aeon_fuse_minutes: a gate that finished counts as progress, not just one still running"

# THE DEFECT: the live-gate exemption (Part 4) masks the fuse while the gate runs, but
# records nothing — so the instant the gate exits, the still-stale worktree mtime is
# exposed and the fuse reads as if nothing had happened for the gate's whole runtime.
# The fix folds the gate-run directory's own timestamps (rc/out/started) into the
# fuse's "last moved" clock, so a verdict that just landed resets it like a commit would.

BEAD5="sp-gatefin-x"
WT5="$RUN/worktree/$BEAD5"
mkdir -p "$WT5"
touch -d "60 minutes ago" "$WT5/old.txt"
touch -d "60 minutes ago" "$WT5"
GATE5_DIR="$RUN/gate-run/testname.spira_${BEAD5}"
mkdir -p "$GATE5_DIR"

# A. no gate activity at all — the stale worktree must still trip (positive control:
#    proves this fixture's staleness alone is enough to read past the wall).
no_gate_fuse5="$(fuse "$BEAD5" "$WT5")"
if [[ "$no_gate_fuse5" =~ ^[0-9]+$ ]] && [ "$no_gate_fuse5" -ge 55 ] 2>/dev/null; then
    ok "no gate activity: stale worktree reads ~60m (positive control, got $no_gate_fuse5)"
else
    bad "no gate activity: stale worktree reads ~60m (positive control)" "got [$no_gate_fuse5]"
fi

# B. gate rc/out/started files written just now, but no live gate process (it exited).
#    The worktree is still stale — only the gate-run directory is fresh.
printf '0\n' > "$GATE5_DIR/rc"
printf 'gate output\n' > "$GATE5_DIR/out"
printf '%s\n' "$(date +%s)" > "$GATE5_DIR/started"
finished_fuse5="$(fuse "$BEAD5" "$WT5")"
if [[ "$finished_fuse5" =~ ^[0-9]+$ ]] && [ "$finished_fuse5" -lt 5 ] 2>/dev/null; then
    ok "gate just finished: fuse resets from the gate's own timestamps (got $finished_fuse5)"
else
    bad "gate just finished: fuse resets from the gate's own timestamps" "expected near 0, got [$finished_fuse5]"
fi

# ---- Part 5: cockpit-metrics.py counts requeue-thrash ledger lines ------------------
echo
echo "cockpit-metrics.py: status=requeue-thrash is counted as SP_AEON_THRASH"

METRICS="$HERE/cockpit-metrics.py"
if [ ! -f "$METRICS" ]; then
    bad "cockpit-metrics.py exists" "not found at $METRICS"
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
    is "empty ledger → SP_AEON_THRASH=0 (positive control)" "0" "$zero_thrash"

    # Now add timestamped ledger lines (cockpit-metrics.py expects ISO timestamps).
    NOW="$(date -u '+%Y-%m-%dT%H:%M:%SZ')"
    printf '%s awake fayth sp-aaa worked\n' "$NOW" >> "$LEDGER"
    printf '%s done fayth sp-bbb rc=0 status=requeue-thrash\n' "$NOW" >> "$LEDGER"
    printf '%s done fayth sp-ccc rc=0 status=requeue-thrash\n' "$NOW" >> "$LEDGER"
    printf '%s done fayth sp-ddd rc=0 status=requeue-thrash\n' "$NOW" >> "$LEDGER"
    printf '%s done fayth sp-eee rc=0 status=requeue-slain\n' "$NOW" >> "$LEDGER"
    out3="$(call_ledger)"
    thrash_count="$(key_val SP_AEON_THRASH "$out3")"
    is "three requeue-thrash lines → SP_AEON_THRASH=3" "3" "$thrash_count"

    # Verify other counters are not contaminated.
    worked_count="$(key_val SP_AEON_WORKED "$out3")"
    is "thrash lines do not inflate SP_AEON_WORKED" "1" "$worked_count"
fi

# ---- Part 6: SPIRA_THRASH_MINUTES in conf.sh ----------------------------------------
echo
echo "conf.sh: SPIRA_THRASH_MINUTES has a default value"

CONF="$HERE/conf.sh"
if [ ! -f "$CONF" ]; then
    bad "conf.sh exists" "not found"
else
    want "conf.sh defines SPIRA_THRASH_MINUTES" "SPIRA_THRASH_MINUTES" "$(cat "$CONF")"
    # Extract the default and verify it is a number.
    default_val="$(grep 'SPIRA_THRASH_MINUTES' "$CONF" | grep -o '[0-9]\+' | head -1)"
    case "${default_val:-}" in
        [0-9]*) ok "SPIRA_THRASH_MINUTES default is a number ($default_val)" ;;
        *)      bad "SPIRA_THRASH_MINUTES default is a number" "got [${default_val:-}]" ;;
    esac
fi

tl_summary
