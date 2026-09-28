#!/usr/bin/env bash
#
# test-aeon-world-stop.sh — the world-stop fence in aeon.sh: a bead labelled
#   SPIRA_WORLD_STOP_LABEL needs the world halted while it runs, but must not run world.sh
#   stop out from under live aeons. The near-miss this closes: sp-6ylz had "needs the world
#   stopped" in its title, was dispatchable anyway, and an aeon ran world.sh stop with live
#   aeons running, producing a three-minute write outage. (sp-ynvd)
#
# RETIRED (sp-j89pd, wave 4.2): world_stop_decide and bead_has_label (lib.sh) had zero live
# callers — the decision is now aeon::decide::world_stop and the poison-race label check is
# aeon::bd::has_label (aeon/src/decide.rs, aeon/src/bd.rs). Their T1 tables here are deleted
# with them; T3 below still proves the wiring end to end through the real `aeon` binary.
#
# defect: sp-ynvd
# tier: T2
# covers: aeon/src/* spira-world/src/bin/world.rs spira/conf.sh spira/lib.sh UC-aeon-execution-04
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

echo "test-aeon-world-stop.sh"

# ===========================================================================================
echo
echo "T3: one real aeon run proves the wiring — stop precedes start"
# ===========================================================================================
# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-aeon-world-stop
LIVE_PID=""
trap 'kill "$LIVE_PID" 2>/dev/null; wait "$LIVE_PID" 2>/dev/null || true; testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up aeonsworld || { echo "test-aeon-world-stop: could not build a fixture database"; exit 1; }
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

ORIGIN="$TMP/origin.git"; git init -q --bare -b main "$ORIGIN"
REPO="$TMP/repo"; git clone -q "$ORIGIN" "$REPO" 2>/dev/null
git -C "$REPO" config user.email t@t; git -C "$REPO" config user.name t
printf 'seed\n' > "$REPO/f"
git -C "$REPO" add f; git -C "$REPO" commit -qm seed; git -C "$REPO" push -q origin main 2>/dev/null

export SPIRA_HOME="$TMP/home"; mkdir -p "$SPIRA_HOME/chamber"
cp "$HERE/lib.sh" "$HERE/conf.sh" "$SPIRA_HOME/"
cp -r "$HERE/conf.d" "$HERE/conf-gen.sh" "$SPIRA_HOME/"
export SPIRA_RUN="$TMP/run"; mkdir -p "$SPIRA_RUN"
export SPIRA_REPO_MAP="$TMP/repo-map"
printf 'fixture | %s | push | origin/main | |\n' "$REPO" > "$SPIRA_REPO_MAP"

cat > "$SPIRA_HOME/chamber/builder.fayth" <<FAYTH
FAYTH_NAME=builder
FAYTH_LABELS="\${SPIRA_SCOPE_LABEL:+\${SPIRA_SCOPE_LABEL},}\${SPIRA_PLAN_LABEL}"
FAYTH_EXCLUDE_LABELS="spira-poison,needs-operator"
FAYTH_MAX_CONCURRENT=1
FAYTH_HEARTBEAT_SECONDS=600
FAYTH
printf 'work {{BEAD_ID}} in {{REPO}} on {{BRANCH}}\n{{PARK}}\n' > "$SPIRA_HOME/chamber/builder.md"

BIN="$TMP/bin"; mkdir -p "$BIN"
export SPIRA_AGENT="$BIN/claude" TMP
command -v aeon >/dev/null 2>&1 \
    || { echo "test-aeon-world-stop: aeon is not on PATH — refusing to run the real model" >&2; exit 1; }
cat > "$BIN/claude" <<'SHIM'
#!/usr/bin/env bash
cat /dev/stdin > "$TMP/prompt"
id="$(sed -n 's/^work \(sp-[a-z0-9-]*\) .*/\1/p' "$TMP/prompt" | head -1)"
printf 'my work\n' >> f
git add -A && git -c user.email=a@a -c user.name=aeon commit -qm "$id — the work"
bd -C "$SPIRA_DB" close "$id" --reason "done" >/dev/null 2>&1
printf '{"type":"result","subtype":"success","is_error":false,"result":"done","num_turns":3}\n'
exit 0
SHIM
chmod +x "$BIN/claude"

WORLD_CALLS="$TMP/world-calls"; : > "$WORLD_CALLS"
cat > "$SPIRA_HOME/world.sh" <<'WORLDSTUB'
#!/usr/bin/env bash
printf '%s\n' "$1" >> "$WORLD_CALLS"
exit 0
WORLDSTUB
chmod +x "$SPIRA_HOME/world.sh"
export WORLD_CALLS
printf '#!/usr/bin/env bash\nexit 0\n' > "$SPIRA_HOME/slay.sh"; chmod +x "$SPIRA_HOME/slay.sh"

seed() {
    local labels="${SPIRA_SCOPE_LABEL:+${SPIRA_SCOPE_LABEL},}${SPIRA_PLAN_LABEL:-plan},repo:fixture,world-stop"
    printf '{"id":"%s","title":"t","status":"open","issue_type":"task","labels":["%s"],"updated_at":"2026-09-08T00:00:00Z"}\n' \
        "$1" "$(printf '%s' "$labels" | sed 's/,/","/g')" | testdb_seed
}
field() { bd -C "$SPIRA_DB" show "$1" --json 2>/dev/null | sed -n '/^[[{]/,$p' | python3 -c '
import sys,json
d=json.load(sys.stdin); d=d if isinstance(d,list) else [d]; print(d[0].get(sys.argv[1]) or "")' "$2" 2>/dev/null; }
run_aeon() { : > "$WORLD_CALLS"; rm -rf "$SPIRA_RUN/worktree"; PATH="$SPIRA_HOME:$PATH" aeon --home "$SPIRA_HOME" builder > "$TMP/out" 2>&1; }

testdb_reset; seed sp-ws-3
run_aeon
world_calls="$(cat "$WORLD_CALLS")"
is   "bead is converted to submitted, not left closed" open "$(field sp-ws-3 status)"
want "world.sh stop was called"        "stop"  "$world_calls"
want "world.sh start was called"       "start" "$world_calls"
stop_line="$(grep -n 'stop'  "$WORLD_CALLS" | head -1 | cut -d: -f1)"
start_line="$(grep -n 'start' "$WORLD_CALLS" | head -1 | cut -d: -f1)"
if [ -n "$stop_line" ] && [ -n "$start_line" ] && [ "$stop_line" -lt "$start_line" ]; then
    ok "stop precedes start in the call log"
else
    bad "stop precedes start in the call log" "stop=$stop_line start=$start_line in $(cat "$WORLD_CALLS")"
fi

tl_summary
