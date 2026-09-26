#!/usr/bin/env bash
#
# test-cockpit-fuse.sh — liveness fuse on every working aeon in the ops pane.
#
#   ./test-cockpit-fuse.sh
#
# The fuse is a repository signal, not a session signal: it measures how long
# since the aeon's deliverable (a commit ahead of the base ref, or a file write
# in the worktree) last moved. Three new keys per aeon:
#
#   SP_AEON<n>_FUSE   minutes since the deliverable moved ("gate" if a gate runs, "?" if unreadable)
#   SP_AEON<n>_WALL   total wall minutes (same figure as MIN, stored separately for sp-cuvi)
#
# A turn boundary is also no longer rendered as "session ended: success" — a
# result event in trace_tail now reads as "turn N: success" to distinguish a
# turn boundary from a terminal state.
#
# defect: sp-yhue
# covers: spira/cockpit.sh cockpit/health.sh spira/lib.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"
PANE="$HERE/../cockpit/health.sh"
TMP="$(mktemp -d)"; trap 'kill_all 2>/dev/null; rm -rf "$TMP"' EXIT
RUN="$TMP/run"
mkdir -p "$RUN"
PIDS=()

# kill_all: clean up any background processes created during the suite.
kill_all() { [ "${#PIDS[@]}" -gt 0 ] && kill "${PIDS[@]}" 2>/dev/null; wait 2>/dev/null; }

# field <output> <key> -> the value of SP_AEON0_<KEY> or the bare key
field() { sed -n "s/^$2=//p" <<< "$1" | head -1; }

# ---- cockpit now seam: the function the suite tests directly -------------------------
# `cockpit.sh now` runs now_keys() and prints the keys to stdout. No write is needed here
# because the writer fence requires the systemd invocation id, which the test does not have.
# SPIRA_COCKPIT_FORCE is not needed for the `now` subcommand — it bypasses cockpit_may_write.
run_now() {
    # AN EXPLICIT MINIMAL ENVIRONMENT. Ambient configuration silently decides verdicts:
    # a suite that inherits a real spira.conf would assert against one box. Pass only what
    # now_keys needs (law-gates-run-in-a-clean-environment).
    # BD_TIMEOUT=1: bdjson calls fail fast against a nonexistent database. The default of
    # 180s per call would stall 3 aeons × 2 calls = 6 minutes of waiting for nothing.
    env -i PATH="$PATH" HOME="$TMP" LC_ALL=C.UTF-8 \
        SPIRA_CONF="$TMP/no.conf" SPIRA_HOME="$HERE" SPIRA_REPO="$TMP" \
        SPIRA_RUN="$RUN" SPIRA_DB="$TMP/nodb" \
        SPIRA_REPO_MAP="$TMP/no-map" BD_TIMEOUT=1 \
        "$@" \
        bash "$HERE/cockpit.sh" now 2>/dev/null
}

# make_aeon <bead> -> start a fake aeon process and write its pid file.
# exec -a renames argv[0] so /proc/<pid>/cmdline contains "aeon.sh", exactly as aeon_alive checks.
# Called without $(...) so the background job is a child of the main script, not a subshell.
# A bash $(...) subshell waits for ALL its children before exiting — calling this from $()
# would hang until sleep 9999 dies even though the function would have "returned".
make_aeon() {
    local bead="$1"
    bash -c 'exec -a "aeon.sh" sleep 9999' &
    local pid="$!"
    PIDS+=("$pid")
    echo "$pid" > "$RUN/aeon-builder-${bead}.pid"
}

# make_gate <bead> -> start a fake gate process and write the gate-run pid file.
# "gate.sh" in argv[0] is what the gate detection logic checks via /proc cmdline.
# Called without $(...) — same reason as make_aeon. The caller reads the pid from the file
# the gate writes rather than capturing it via command substitution.
make_gate() {
    local bead="$1"
    bash -c 'exec -a "gate.sh" sleep 9999' &
    local pid="$!"
    PIDS+=("$pid")
    local gate_dir="$RUN/gate-run/testname.spira_${bead}"
    mkdir -p "$gate_dir"
    echo "$pid" > "$gate_dir/pid"
}

# make_worktree <bead> -> create a minimal git worktree in $RUN/worktree/<bead>.
# Writes a file and makes an initial commit so both mtime and git log signals exist.
make_worktree() {
    local bead="$1"
    local wt="$RUN/worktree/$bead"
    mkdir -p "$wt"
    git -C "$wt" init -q
    git -C "$wt" config user.email "t@t" && git -C "$wt" config user.name "T"
    echo "init" > "$wt/file.txt"
    git -C "$wt" add file.txt
    git -C "$wt" commit -q -m "initial"
}

# ---- Part 1: collector emits FUSE and WALL ------------------------------------------
echo "cockpit now_keys: FUSE and WALL keys are emitted for each working aeon"

# POSITIVE CONTROL FIRST. No aeons → no SP_AEON0_FUSE emitted.
out_empty="$(run_now)"
if grep -q 'SP_AEON0_FUSE' <<< "$out_empty"; then
    bad "no aeons → no SP_AEON0_FUSE (positive control failed: the key was emitted)"
else
    ok "no aeons → no SP_AEON0_FUSE (positive control)"
fi

# Single aeon with a worktree: the fuse is a number (file was just written by make_worktree).
BEAD1="sp-ftest1"
make_aeon "$BEAD1"
make_worktree "$BEAD1"
out1="$(run_now)"
fuse1="$(field "$out1" SP_AEON0_FUSE)"
wall1="$(field "$out1" SP_AEON0_WALL)"
case "$fuse1" in
    [0-9]*) ok "a just-written worktree produces a numeric fuse" ;;
    *)      bad "a just-written worktree produces a numeric fuse: got [$fuse1]" ;;
esac
case "$wall1" in
    [0-9]*) ok "WALL is a number (wall minutes since aeon start)" ;;
    *)      bad "WALL is a number: got [$wall1]" ;;
esac
# The fuse must be 0 or very small — we just wrote a file.
is "a just-written worktree has fuse 0" "0" "$fuse1"

# ---- Part 2: missing worktree renders ? ---------------------------------------------
echo
echo "cockpit now_keys: missing worktree renders FUSE=?"

BEAD2="sp-ftest2"
make_aeon "$BEAD2"
# No worktree created for BEAD2.
out2="$(run_now)"
# Find which index holds BEAD2.
fuse2=""
for idx in 0 1 2; do
    b="$(field "$out2" "SP_AEON${idx}_BEAD")"
    [ "$b" = "$BEAD2" ] && fuse2="$(field "$out2" "SP_AEON${idx}_FUSE")" && break
done
# THE POSITIVE CONTROL: verify the bead key was found at all. A ? that comes from
# an absent key looks identical to a ? from a computed read failure — only knowing
# the key was present distinguishes them (law-absence-needs-a-positive-control).
if grep -q "SP_AEON.*_BEAD=$BEAD2" <<< "$out2"; then
    ok "SP_AEON?_BEAD=$BEAD2 was found in output (positive control)"
else
    bad "SP_AEON?_BEAD=$BEAD2 was not found in output — bead key missing entirely"
fi
is "no worktree renders FUSE=?" "?" "$fuse2"

# ---- Part 3: gate suppresses the fuse -----------------------------------------------
echo
echo "cockpit now_keys: gate running for this bead yields FUSE=gate"

BEAD3="sp-ftest3"
make_aeon "$BEAD3"
make_worktree "$BEAD3"
make_gate "$BEAD3"
GATE3_PID="$(cat "$RUN/gate-run/testname.spira_${BEAD3}/pid")"

out3="$(run_now)"
fuse3=""
for idx in 0 1 2 3; do
    b="$(field "$out3" "SP_AEON${idx}_BEAD")"
    [ "$b" = "$BEAD3" ] && fuse3="$(field "$out3" "SP_AEON${idx}_FUSE")" && break
done
# POSITIVE CONTROL: prove the bead is in the output before asserting its fuse.
if grep -q "SP_AEON.*_BEAD=$BEAD3" <<< "$out3"; then
    ok "SP_AEON?_BEAD=$BEAD3 found (positive control)"
else
    bad "SP_AEON?_BEAD=$BEAD3 not found — bead key missing entirely"
fi
is "live gate for this bead yields FUSE=gate" "gate" "$fuse3"

# Gate detection requires a LIVE process. Kill the gate and verify the fuse resumes
# computing normally — a dead pid must not render as gate, because it would be the
# all-clear reading a broken check must never produce.
kill "$GATE3_PID" 2>/dev/null; wait "$GATE3_PID" 2>/dev/null || true
out3b="$(run_now)"
fuse3b=""
for idx in 0 1 2 3; do
    b="$(field "$out3b" "SP_AEON${idx}_BEAD")"
    [ "$b" = "$BEAD3" ] && fuse3b="$(field "$out3b" "SP_AEON${idx}_FUSE")" && break
done
case "$fuse3b" in
    [0-9]*) ok "dead gate pid: fuse resumes as a number" ;;
    gate)   bad "dead gate pid: fuse still reads 'gate'" ;;
    ?)      ok "dead gate pid: fuse reads ? (worktree check returned nothing, acceptable)" ;;
    *)      bad "dead gate pid: unexpected fuse value [$fuse3b]" ;;
esac

# ---- Part 4: trace_tail turn boundary -----------------------------------------------
echo
echo "trace_tail: result event renders as turn index, not 'session ended'"

TD="$TMP/trace"; mkdir -p "$TD"
cat > "$TD/sp-turn.log" <<'TRACE'
{"type":"assistant","message":{"id":"m1","content":[{"type":"tool_use","name":"Bash","input":{"command":"ls"}}]}}
{"type":"result","subtype":"success","session_id":"s1","is_error":false}
{"type":"assistant","message":{"id":"m2","content":[{"type":"tool_use","name":"Read","input":{"file_path":"/tmp/x"}}]}}
{"type":"result","subtype":"success","session_id":"s1","is_error":false}
TRACE

tail_out="$(env -i PATH="$PATH" HOME="$TMP" LC_ALL=C.UTF-8 SPIRA_CONF="$TMP/no.conf" \
    bash -c '. "$1"/lib.sh; trace_tail "$2" 10' _ "$HERE" "$TD/sp-turn.log" 2>/dev/null)"

# A result event must render as "turn N: ..." not "session ended: ...".
if grep -q 'session ended' <<< "$tail_out"; then
    bad "result events must not render as 'session ended'"
else
    ok "result events do not render as 'session ended'"
fi
if grep -q 'turn 1' <<< "$tail_out"; then
    ok "first result event renders as 'turn 1'"
else
    bad "first result event did not render as 'turn 1': output was [$tail_out]"
fi
if grep -q 'turn 2' <<< "$tail_out"; then
    ok "second result event renders as 'turn 2'"
else
    bad "second result event did not render as 'turn 2': output was [$tail_out]"
fi

# ---- Part 5: rendering in health.sh -------------------------------------------------
echo
echo "health.sh: lease countdown renders on the bead row"

if [ ! -f "$PANE" ]; then
    bad "cannot find pane at $PANE"
else

PD="$TMP/pane"; mkdir -p "$PD/home" "$PD/bin"
SNAPF="$PD/cockpit.env"
printf '#!/bin/sh\necho active\n' > "$PD/bin/mock-systemctl"
chmod +x "$PD/bin/mock-systemctl"

snap() {
    python3 -c '
import sys
for line in sys.stdin.read().splitlines():
    if not line.strip(): continue
    k, _, v = line.partition("=")
    print("%s=%s" % (k, "\x27" + v.replace("\x27", "\x27\\\x27\x27") + "\x27"))
' > "$SNAPF"
}

pane() {
    # SPIRA_RUN="$PD" so health.sh reads $PD/cockpit.env (which snap() writes).
    env -i PATH="$PATH" HOME="$PD/home" TERM=dumb LC_ALL=C.UTF-8 \
        SPIRA_CONF="$TMP/no.conf" SPIRA_REPO="$TMP" SPIRA_RUN="$PD" \
        SPIRA_SYSTEMCTL="$PD/bin/mock-systemctl" \
        bash "$PANE" once "${1:-0}" "${2:-0}" 2>/dev/null \
      | sed 's/\x1b\[[?0-9;]*[a-zA-Z]//g'
}

# LEASE COUNTDOWN renders as "Nm" on the bead row (time left in the liveness lease).
{ printf 'SP_AEON_N=1\nSP_AEON0_NAME=valefor\nSP_AEON0_FAYTH=builder\nSP_AEON0_BEAD=sp-fuse\n'
  printf 'SP_AEON0_MIN=20\nSP_AEON0_TURNS=37\nSP_AEON0_CTX=100000\nSP_AEON0_FILES=3\n'
  printf 'SP_AEON0_QUIET=60\nSP_AEON0_ACT=Bash test\nSP_AEON0_TITLE=fuse test bead\n'
  printf 'SP_AEON0_LEASE=10\nSP_NEXT_N=0\nSP_AWAITING_N=0\n'; } | snap
row_num="$(pane 0)"
if grep -q '10m' <<< "$row_num"; then
    ok "lease countdown renders as '10m'"
else
    bad "lease countdown did not render '10m': $(printf '%s\n' "$row_num" | grep 'sp-fuse')"
fi

# LOW LEASE (<3m) renders (still as "Nm" — the colour changes, the format does not).
{ printf 'SP_AEON_N=1\nSP_AEON0_NAME=valefor\nSP_AEON0_FAYTH=builder\nSP_AEON0_BEAD=sp-gate\n'
  printf 'SP_AEON0_MIN=20\nSP_AEON0_TURNS=37\nSP_AEON0_CTX=100000\nSP_AEON0_FILES=3\n'
  printf 'SP_AEON0_QUIET=60\nSP_AEON0_ACT=Bash test\nSP_AEON0_TITLE=gate test bead\n'
  printf 'SP_AEON0_LEASE=1\nSP_NEXT_N=0\nSP_AWAITING_N=0\n'; } | snap
row_gate="$(pane 0)"
if grep -q '1m' <<< "$row_gate"; then
    ok "low lease renders as '1m'"
else
    bad "low lease did not render '1m': $(printf '%s\n' "$row_gate" | grep 'sp-gate')"
fi

# MISSING LEASE (key absent from snapshot) renders as "?".
{ printf 'SP_AEON_N=1\nSP_AEON0_NAME=valefor\nSP_AEON0_FAYTH=builder\nSP_AEON0_BEAD=sp-miss\n'
  printf 'SP_AEON0_MIN=20\nSP_AEON0_TURNS=37\nSP_AEON0_CTX=100000\nSP_AEON0_FILES=3\n'
  printf 'SP_AEON0_QUIET=60\nSP_AEON0_ACT=Bash test\nSP_AEON0_TITLE=missing fuse bead\n'
  printf 'SP_NEXT_N=0\nSP_AWAITING_N=0\n'; } | snap
row_miss="$(pane 0)"
# POSITIVE CONTROL: the bead row must exist before checking the lease marker.
if grep -q 'sp-miss' <<< "$row_miss"; then
    ok "bead row renders when LEASE key is absent (positive control)"
else
    bad "bead row absent — cannot check lease fallback"
fi
if grep -q '?' <<< "$(printf '%s\n' "$row_miss" | grep 'sp-miss')"; then
    ok "absent LEASE key renders as '?' on bead row"
else
    bad "absent LEASE key did not render '?': $(printf '%s\n' "$row_miss" | grep 'sp-miss')"
fi

# Fitting the bead row to 70/96 columns is test-now.sh's job (cluster 8, docs/test-plan/
# cockpit-observability.md, row 22): it already golden-frames row width at several pane
# sizes, and a second copy here tested the same "no row exceeds the pane" property, not
# anything specific to the lease field.

fi  # pane exists

echo
tl_summary
