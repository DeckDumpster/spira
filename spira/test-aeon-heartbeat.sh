#!/usr/bin/env bash
#
# test-aeon-heartbeat.sh — the heartbeat reads work, not mere existence.
#
#   ./test-aeon-heartbeat.sh
#
# The heartbeat's job is to tell "the model is doing work" from "the process is alive". The
# old signal — log file growth — could not: tool_progress heartbeats write to the log every
# ~30s while the model is blocked, so a fully stuck aeon's log grows steadily. These cases
# cover the signal that replaced it: elapsed_time_seconds on the trailing heartbeat, the
# process subtree check, and the flock exemption.
#
# No database, no network, under a second.
#
# CONTAINER. An isolated PID namespace prevents ambient flock processes from interfering
# with the subtree_has_flock cases. When invoked on the host (IN_TESTENV unset), this
# script starts testenv, re-runs itself inside (IN_TESTENV=1), then tears the container
# down. Inside the container the block is skipped and the assertions run directly.
#
# defect: sp-q697
# covers: spira/lib.sh spira/aeon.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"

if [ "${IN_TESTENV:-}" != "1" ]; then
    command -v podman >/dev/null 2>&1 || {
        printf 'SKIP test-aeon-heartbeat.sh: podman not found on PATH\n' >&2
        exit 77
    }
    TESTENV="$HERE/testenv.sh"
    CNAME="spira-testenv-hb-$$"
    bash "$TESTENV" up --name "$CNAME" >&2
    rc=0
    bash "$TESTENV" exec --name "$CNAME" -- \
        env IN_TESTENV=1 bash /workspace/spira/test-aeon-heartbeat.sh || rc=$?
    bash "$TESTENV" down --name "$CNAME" >/dev/null 2>&1 || true
    exit "$rc"
fi

pass=0; fail=0
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT
_bg_pids=()   # all background PIDs spawned here; checked for cleanup at suite exit

is() {
    if [ "$2" = "$3" ]; then
        pass=$((pass+1)); printf '  ok    %s\n' "$1"
    else
        fail=$((fail+1)); printf '  FAIL  %s: want [%s] got [%s]\n' "$1" "$2" "$3"
    fi
}

# Source lib.sh in a clean environment.
hmi() {
    env -i PATH="$PATH" HOME="$TMP" LC_ALL=C.UTF-8 SPIRA_CONF="$TMP/no.conf" \
        bash -c '. "$1"/lib.sh; heartbeat_model_idle "$2"' _ "$HERE" "$1" 2>/dev/null
}

echo "heartbeat_model_idle — the signal that tells blocked from stuck"

# Case 1: an assistant event — the model just acted.
printf '{"type":"assistant","message":{"id":"m1","content":[{"type":"text","text":"hello"}]}}\n' \
    > "$TMP/acting.log"
read -r idle state < <(hmi "$TMP/acting.log")
is "an assistant event is acting"  "acting"  "$state"
is "and idle time is 0"            "0"       "$idle"

# Case 2: a tool_progress heartbeat — the model is blocked on a tool call.
printf '{"type":"tool_progress","heartbeat":true,"elapsed_time_seconds":510,"tool_name":"Bash"}\n' \
    > "$TMP/blocked.log"
read -r idle state < <(hmi "$TMP/blocked.log")
is "a tool_progress heartbeat is blocked"  "blocked"  "$state"
is "and carries its elapsed_time_seconds"   "510"      "$idle"

# Case 3: a tool_progress heartbeat with zero elapsed — just started blocking.
printf '{"type":"tool_progress","heartbeat":true,"elapsed_time_seconds":0,"tool_name":"Bash"}\n' \
    > "$TMP/fresh-block.log"
read -r idle state < <(hmi "$TMP/fresh-block.log")
is "a fresh block is still blocked"   "blocked"  "$state"
is "with zero idle"                    "0"        "$idle"

# Case 4: a tool_result event — the model is about to act.
printf '{"type":"user","message":{"content":[{"type":"tool_result","content":"ok"}]}}\n' \
    > "$TMP/result.log"
read -r idle state < <(hmi "$TMP/result.log")
is "a tool_result is acting (model is next)" "acting" "$state"

# Case 5: a trace mark only — the model has not produced anything yet.
printf '=== spira attempt 1 aeon=valefor at=2026-01-01T00:00:00Z kept=0\n' \
    > "$TMP/silent.log"
read -r idle state < <(hmi "$TMP/silent.log")
is "a trace mark alone is silent" "silent" "$state"
# idle should be a large number (seconds since 2026-01-01).
[ "$idle" -gt 0 ] && { pass=$((pass+1)); printf '  ok    and its idle is positive\n'; } \
    || { fail=$((fail+1)); printf '  FAIL  trace mark idle should be positive, got [%s]\n' "$idle"; }

# Case 6: an unparseable line.
printf 'this is not json and not a trace mark\n' > "$TMP/garbage.log"
read -r idle state < <(hmi "$TMP/garbage.log")
is "an unparseable line is ?"  "?"   "$state"
is "with idle -1"              "-1"  "$idle"

# Case 7: an empty file.
: > "$TMP/empty.log"
read -r idle state < <(hmi "$TMP/empty.log")
is "an empty file is ?"  "?"   "$state"
is "with idle -1"         "-1"  "$idle"

# Case 8: a non-heartbeat tool_progress (no heartbeat flag).
printf '{"type":"tool_progress","elapsed_time_seconds":100}\n' > "$TMP/nohb.log"
read -r idle state < <(hmi "$TMP/nohb.log")
is "a tool_progress without heartbeat flag is acting" "acting" "$state"

# Case 9: a nonexistent file.
read -r idle state < <(hmi "$TMP/no-such-file.log")
is "a missing file is ?"  "?"   "$state"

# Case 10: a log with multiple lines — only the LAST matters. Even if the model was acting
# earlier, a trailing heartbeat means it is now blocked.
{
    printf '{"type":"assistant","message":{"id":"m1","content":[{"type":"text","text":"hi"}]}}\n'
    printf '{"type":"tool_progress","heartbeat":true,"elapsed_time_seconds":300,"tool_name":"Bash"}\n'
} > "$TMP/multi.log"
read -r idle state < <(hmi "$TMP/multi.log")
is "multiple lines: the trailing heartbeat wins" "blocked" "$state"
is "and carries its own elapsed time"            "300"     "$idle"

echo
echo "youngest_in_subtree — process subtree advancement"

# Spawn a short-lived child whose subtree we can measure.
sleep 3600 &
child_pid=$!
_bg_pids+=("$child_pid")
# Give /proc a moment to populate.
sleep 0.1

yis() {
    env -i PATH="$PATH" HOME="$TMP" LC_ALL=C.UTF-8 SPIRA_CONF="$TMP/no.conf" \
        bash -c '. "$1"/lib.sh; youngest_in_subtree "$2" "${3:-0}"' _ "$HERE" "$1" "${2:-}" 2>/dev/null
}

youngest="$(yis "$$")"
now="$(date +%s)"
if [ "$youngest" -gt 0 ] && [ $((now - youngest)) -lt 10 ]; then
    pass=$((pass+1)); printf '  ok    youngest_in_subtree finds a recent child of $$\n'
else
    fail=$((fail+1)); printf '  FAIL  youngest_in_subtree: want recent, got age %ss (youngest=%s now=%s)\n' \
        "$((now - youngest))" "$youngest" "$now"
fi

# Excluding $$ itself from a walk rooted at 1 should exclude all our children.
# Start a subprocess whose only child is one `sleep` — a tree we fully control.
bash -c 'sleep 3600 & wait' &
outer=$!
_bg_pids+=("$outer")
sleep 0.2
youngest_outer="$(yis "$outer")"
if [ "$youngest_outer" -gt 0 ]; then
    pass=$((pass+1)); printf '  ok    youngest_in_subtree finds the nested sleep\n'
else
    fail=$((fail+1)); printf '  FAIL  youngest_in_subtree missed the nested sleep (got %s)\n' "$youngest_outer"
fi
# Save children BEFORE killing outer; once outer dies they reparent to PID 1 and
# pkill -P can no longer find them by parent — which is how they leaked.
outer_children=$(pgrep -P "$outer" 2>/dev/null || true)
_bg_pids+=($outer_children)
kill "$outer" 2>/dev/null; wait "$outer" 2>/dev/null || true
for _p in $outer_children; do kill "$_p" 2>/dev/null || true; done
for _p in $outer_children; do wait "$_p" 2>/dev/null || true; done

# A PID that does not exist.
youngest_none="$(yis "99999999")"
is "a nonexistent root finds nothing" "0" "$youngest_none"

kill "$child_pid" 2>/dev/null; wait "$child_pid" 2>/dev/null || true

echo
echo "subtree_has_flock — the fixture queue exemption"

shf() {
    env -i PATH="$PATH" HOME="$TMP" LC_ALL=C.UTF-8 SPIRA_CONF="$TMP/no.conf" \
        bash -c '. "$1"/lib.sh; subtree_has_flock "$2"' _ "$HERE" "$1" 2>/dev/null
}

# This shell has no flock child.
if shf "$$"; then
    fail=$((fail+1)); printf '  FAIL  subtree_has_flock found a flock where there is none\n'
else
    pass=$((pass+1)); printf '  ok    no flock in a clean subtree\n'
fi

# Spawn a flock child.
touch "$TMP/lockfile"
flock "$TMP/lockfile" sleep 3600 &
flock_parent=$!
_bg_pids+=("$flock_parent")
sleep 0.1

if shf "$$"; then
    pass=$((pass+1)); printf '  ok    subtree_has_flock finds the flock child\n'
else
    fail=$((fail+1)); printf '  FAIL  subtree_has_flock missed a flock child\n'
fi

# flock forks its command as a child rather than exec'ing it, so killing flock leaves
# the sleep child alive. Save children first and kill them after.
flock_children=$(pgrep -P "$flock_parent" 2>/dev/null || true)
_bg_pids+=($flock_children)
kill "$flock_parent" 2>/dev/null; wait "$flock_parent" 2>/dev/null || true
for _p in $flock_children; do
    kill "$_p" 2>/dev/null || true
    # kill is async; poll until dead since the child is now orphaned (not our descendant).
    _i=0; while kill -0 "$_p" 2>/dev/null && [ "$_i" -lt 20 ]; do sleep 0.01; _i=$((_i+1)); done
done

echo
echo "subprocess cleanup — no background children outlive the suite"
_leaked=0
for _pid in "${_bg_pids[@]}"; do
    if kill -0 "$_pid" 2>/dev/null; then
        _leaked=$((_leaked + 1))
        fail=$((fail + 1))
        printf '  FAIL  subprocess %s still alive after cleanup\n' "$_pid"
        kill "$_pid" 2>/dev/null || true
    fi
done
if [ "$_leaked" -eq 0 ]; then
    pass=$((pass + 1))
    printf '  ok    all spawned subprocesses cleaned up\n'
fi

echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
