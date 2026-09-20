#!/usr/bin/env bash
#
# test-aeon-world-stop.sh — the world-stop fence in aeon.sh refuses a world-stop bead
# when live aeons are present, and calls world.sh stop/start when they are not.
#
#   ./test-aeon-world-stop.sh
#
# WHAT THIS SUITE COVERS
# ----------------------
# A bead labelled world-stop (SPIRA_WORLD_STOP_LABEL) declares it needs the world halted
# while it runs. The fence in aeon.sh enforces this: if live aeons exist, it refuses the
# claim rather than letting a session run world.sh stop under them. The near-miss this
# closes: sp-6ylz had "needs the world stopped" in its title, was dispatchable anyway,
# and an aeon ran world.sh stop with live aeons running, producing a three-minute write
# outage. (sp-ynvd)
#
# FOUR PROPERTIES, each a pair (law-absence-needs-a-positive-control):
#
#   1. A bead WITHOUT the world-stop label is claimed and worked normally — no fence fires.
#
#   2. A bead WITH the world-stop label and live aeons present is REFUSED: the claim is
#      released, the refusal message names the override (SPIRA_WORLD_STOP_SKIP) and the
#      live aeon by pidfile name, and world.sh is never called.
#
#   3. A bead WITH the world-stop label and NO live aeons calls world.sh stop before the
#      session and world.sh start after — confirmed by reading the call log, not by asking
#      the real systemd.
#
#   4. With SPIRA_WORLD_STOP_SKIP=1 set (the named override), the fence proceeds even with
#      live aeons present — world.sh stop is still called.
#
# world.sh IS STUBBED so no real systemd units are touched. The stub records every
# positional argument world.sh receives, one call per line. A live-aeon scenario uses a
# REAL background process so /proc scanning produces a live pid.
#
# defect: sp-ynvd
# covers: spira/aeon.sh spira/world.sh spira/conf.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

echo "test-aeon-world-stop.sh"

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-aeon-world-stop
TMP="$(mktemp -d)"
LIVE_PID=""
trap 'kill "$LIVE_PID" 2>/dev/null; wait "$LIVE_PID" 2>/dev/null || true; testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up aeonsworld || { echo "test-aeon-world-stop: could not build a fixture database"; exit 1; }
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

# A repository with a remote, because aeon.sh resolves the base as a remote-tracking ref.
ORIGIN="$TMP/origin.git"; git init -q --bare -b main "$ORIGIN"
REPO="$TMP/repo"; git clone -q "$ORIGIN" "$REPO" 2>/dev/null
git -C "$REPO" config user.email t@t; git -C "$REPO" config user.name t
printf 'seed\n' > "$REPO/f"
git -C "$REPO" add f; git -C "$REPO" commit -qm seed; git -C "$REPO" push -q origin main 2>/dev/null

# Minimal harness tree: aeon.sh, conf.sh, lib.sh, world.sh and their runtime dirs.
export SPIRA_HOME="$TMP/home"; mkdir -p "$SPIRA_HOME/chamber"
cp "$HERE/lib.sh" "$HERE/conf.sh" "$HERE/aeon.sh" "$SPIRA_HOME/"
export SPIRA_RUN="$TMP/run"; mkdir -p "$SPIRA_RUN"
export SPIRA_REPO_MAP="$TMP/repo-map"
printf 'fixture | %s | push | origin/main | |\n' "$REPO" > "$SPIRA_REPO_MAP"

# The stub fayth. FAYTH_STALL_BEATS=1 so the heartbeat exits quickly if we somehow end up
# in a real session wait; FAYTH_HEARTBEAT_SECONDS=1 for the same reason.
cat > "$SPIRA_HOME/chamber/builder.fayth" <<FAYTH
FAYTH_NAME=builder
FAYTH_LABELS="\${SPIRA_SCOPE_LABEL:+\${SPIRA_SCOPE_LABEL},}\${SPIRA_PLAN_LABEL}"
FAYTH_EXCLUDE_LABELS="spira-poison,needs-operator"
FAYTH_MAX_CONCURRENT=1
FAYTH_HEARTBEAT_SECONDS=600
FAYTH

printf 'work {{BEAD_ID}} in {{REPO}} on {{BRANCH}}\n{{PARK}}\n' > "$SPIRA_HOME/chamber/builder.md"

# THE SHIM IS THE SESSION: it closes the bead and exits so aeon.sh sees a normal finish.
# The guard below is not decoration: conf.sh replaces $PATH, so a suite that shimmed
# `claude` by PATH alone would run the real model against the operator's account.
BIN="$TMP/bin"; mkdir -p "$BIN"
export SPIRA_AGENT="$BIN/claude" TMP
grep -q 'SPIRA_AGENT' "$HERE/aeon.sh" \
    || { echo "test-aeon-world-stop: aeon.sh has no SPIRA_AGENT injection point — refusing to run the real model" >&2; exit 1; }
cat > "$BIN/claude" <<'SHIM'
#!/usr/bin/env bash
# Read prompt to get bead id, close the bead, emit a result record.
cat /dev/stdin > "$TMP/prompt"
id="$(sed -n 's/^work \(sp-[a-z0-9-]*\) .*/\1/p' "$TMP/prompt" | head -1)"
printf 'my work\n' >> f
git add -A && git -c user.email=a@a -c user.name=aeon commit -qm "$id — the work"
bd -C "$SPIRA_DB" close "$id" --reason "done" >/dev/null 2>&1
printf '{"type":"result","subtype":"success","is_error":false,"result":"done","num_turns":3}\n'
exit 0
SHIM
chmod +x "$BIN/claude"

# WORLD.SH STUB: records the positional subcommand (stop/start/status) on one line each,
# without reaching real systemd. Sourced with SPIRA_HOME already pointing at our fixture
# tree; a real world.sh sourced from there would still try to use SPIRA_SYSTEMCTL. Instead
# we replace world.sh entirely with a stub that just records calls.
#
# SPIRA_SYSTEMCTL=... is NOT enough here because world.sh is sourced through SPIRA_HOME —
# we replace the whole file so the stub cannot accidentally call the real systemd even if
# conf.sh re-sets PATH.
WORLD_CALLS="$TMP/world-calls"
: > "$WORLD_CALLS"
cat > "$SPIRA_HOME/world.sh" <<'WORLDSTUB'
#!/usr/bin/env bash
# Record the subcommand (first non-flag arg) and exit 0.
printf '%s\n' "$1" >> "$WORLD_CALLS"
exit 0
WORLDSTUB
chmod +x "$SPIRA_HOME/world.sh"
# The stub needs WORLD_CALLS in its environment. We pass it through the exported variable.
export WORLD_CALLS

# slay.sh is called by world.sh stop for any live aeons; stub it so no real aeons are
# slain. (Our own "live aeon" below is just a sleep process, not a real aeon session.)
printf '#!/usr/bin/env bash\nexit 0\n' > "$SPIRA_HOME/slay.sh"; chmod +x "$SPIRA_HOME/slay.sh"

seed() {   # seed <id> [extra-labels...]
    local labels="${SPIRA_SCOPE_LABEL:+${SPIRA_SCOPE_LABEL},}${SPIRA_PLAN_LABEL:-plan},repo:fixture"
    for l in "${@:2}"; do labels="$labels,$l"; done
    printf '{"id":"%s","title":"t","status":"open","issue_type":"task","labels":["%s"],"updated_at":"2026-09-08T00:00:00Z"}\n' \
        "$1" "$(printf '%s' "$labels" | sed 's/,/","/g')" | testdb_seed
}
field() { bd -C "$SPIRA_DB" show "$1" --json 2>/dev/null | sed -n '/^[[{]/,$p' | python3 -c '
import sys,json
d=json.load(sys.stdin); d=d if isinstance(d,list) else [d]; print(d[0].get(sys.argv[1]) or "")' "$2" 2>/dev/null; }
notes() { bd -C "$SPIRA_DB" show "$1" 2>/dev/null | tr '\n' ' '; }
run_aeon() {
    : > "$WORLD_CALLS"
    rm -rf "$SPIRA_RUN/worktree"
    "$SPIRA_HOME/aeon.sh" builder > "$TMP/out" 2>&1
}

# --------------------------------------------------------------------------------------
# 1. BEAD WITHOUT world-stop LABEL — no fence, worked normally
# The positive control: the fence must fire when the label IS present, but must be
# invisible when it is not. If the label-free case also triggered a refusal or a world
# stop, the label check would be meaningless.
# --------------------------------------------------------------------------------------
echo
echo "1. no world-stop label — aeon claims and works the bead normally:"

testdb_reset; seed sp-ws-1   # no world-stop label
run_aeon
out="$(cat "$TMP/out")"
is   "bead is closed"                  closed "$(field sp-ws-1 status)"
nowant "world.sh not called"           "stop"  "$(cat "$WORLD_CALLS")"
nowant "world-stop-fence not logged"   "world-stop-fence" "$out"

# --------------------------------------------------------------------------------------
# 2. BEAD WITH world-stop LABEL + LIVE AEON — fence refuses
# A real process is running whose pidfile is in $SPIRA_RUN, so /proc scanning finds it.
# The fence must: release the claim, log the override name, log the live aeon by pidfile
# name, and leave world.sh uncalled.
# --------------------------------------------------------------------------------------
echo
echo "2. world-stop label + live aeon — fence refuses:"

# Write a pidfile for a real background process, using the aeon pidfile naming convention.
# IMPORTANT: use a fayth name that is NOT "builder". aeon_count("builder") runs BEFORE the
# fence and removes pidfiles whose process is not a live aeon (aeon_alive checks cmdline).
# By naming the fake aeon "ops", aeon_count("builder") scans aeon-builder-*.pid and never
# touches our aeon-ops-*.pid file. The fence scans aeon-*.pid (all), so it still fires.
FAKE_AEON_FAYTH="ops"
FAKE_AEON_ID="sp-ws-live"
FAKE_PIDFILE="$SPIRA_RUN/aeon-${FAKE_AEON_FAYTH}-${FAKE_AEON_ID}.pid"
sleep 300 &
LIVE_PID=$!
printf '%s\n' "$LIVE_PID" > "$FAKE_PIDFILE"

testdb_reset; seed sp-ws-2 "world-stop"
run_aeon
out="$(cat "$TMP/out")"

# Bead must be back on the queue (open, unclaimed) after the refusal.
is   "bead is open after refusal"      open  "$(field sp-ws-2 status)"
is   "claim is released"               ""    "$(field sp-ws-2 assignee)"
want "refusal message names override"  "SPIRA_WORLD_STOP_SKIP" "$out"
want "refusal message names live aeon" "aeon-${FAKE_AEON_FAYTH}-${FAKE_AEON_ID}" "$out"
want "ledger records fence outcome"    "world-stop-fence" "$(cat "$SPIRA_RUN/aeon-ledger.log" 2>/dev/null)"
nowant "world.sh stop not called"      "stop" "$(cat "$WORLD_CALLS")"
# The bead note should also name the live aeon.
want "bead note names live aeon"       "aeon-${FAKE_AEON_FAYTH}-${FAKE_AEON_ID}" "$(notes sp-ws-2)"

# --------------------------------------------------------------------------------------
# 3. BEAD WITH world-stop LABEL + NO LIVE AEON — fence proceeds, calls stop then start
# The pidfile is removed so no live aeon is visible to the /proc scan. The fence must
# call world.sh stop before the session and world.sh start after.
# --------------------------------------------------------------------------------------
echo
echo "3. world-stop label + no live aeon — fence calls world.sh stop/start:"

# Clean up the live aeon so the fence sees no peers.
kill "$LIVE_PID" 2>/dev/null; wait "$LIVE_PID" 2>/dev/null || true; LIVE_PID=""
rm -f "$FAKE_PIDFILE"

testdb_reset; seed sp-ws-3 "world-stop"
run_aeon
world_calls="$(cat "$WORLD_CALLS")"

is   "bead is closed after session"    closed "$(field sp-ws-3 status)"
want "world.sh stop was called"        "stop"  "$world_calls"
want "world.sh start was called"       "start" "$world_calls"
# STOP must precede START — the session ran between them.
stop_line="$(grep -n 'stop'  "$WORLD_CALLS" | head -1 | cut -d: -f1)"
start_line="$(grep -n 'start' "$WORLD_CALLS" | head -1 | cut -d: -f1)"
if [ -n "$stop_line" ] && [ -n "$start_line" ] && [ "$stop_line" -lt "$start_line" ]; then
    ok "stop precedes start in the call log"
else
    bad "stop precedes start in the call log" "stop=$stop_line start=$start_line in $(cat "$WORLD_CALLS")"
fi

# --------------------------------------------------------------------------------------
# 4. BEAD WITH world-stop LABEL + LIVE AEON + OVERRIDE — fence proceeds despite live aeon
# SPIRA_WORLD_STOP_SKIP=1 is the named override. With it set, the fence skips the refusal
# and calls world.sh stop (with the live aeon noted in the log).
# --------------------------------------------------------------------------------------
echo
echo "4. world-stop label + live aeon + SPIRA_WORLD_STOP_SKIP=1 — override proceeds:"

# Restart a live aeon process and its pidfile (same ops/sp-ws-live key as case 2).
sleep 300 &
LIVE_PID=$!
printf '%s\n' "$LIVE_PID" > "$FAKE_PIDFILE"

testdb_reset; seed sp-ws-4 "world-stop"
: > "$WORLD_CALLS"
rm -rf "$SPIRA_RUN/worktree"
SPIRA_WORLD_STOP_SKIP=1 "$SPIRA_HOME/aeon.sh" builder > "$TMP/out" 2>&1
out="$(cat "$TMP/out")"
world_calls="$(cat "$WORLD_CALLS")"

is   "bead is closed despite live aeon (override)" closed "$(field sp-ws-4 status)"
want "world.sh stop was called with override"      "stop"  "$world_calls"
want "world.sh start was called after"             "start" "$world_calls"
want "log notes the override"                      "SPIRA_WORLD_STOP_SKIP" "$out"
nowant "fence refusal not triggered"               "world-stop-fence" "$out"

# Clean up the live process.
kill "$LIVE_PID" 2>/dev/null; wait "$LIVE_PID" 2>/dev/null || true; LIVE_PID=""
rm -f "$FAKE_PIDFILE"

echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
