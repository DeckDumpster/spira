#!/usr/bin/env bash
# tier: T2
# requires: testenv
# covers: spira/sim/** sim/actors.toml sim/durations.toml systemd/spira-sentinel.* systemd/spira-rounds.* systemd/spira-publish.* systemd/spira-gate-worker.*
# lane: sim
# pids: 400
#
# test-sim-happy-path.sh — one bead walked from filed to LANDED, published and tagged, in a
# sim world: `sim run` of spira/sim/scenarios/happy-path.toml (file and claim steps, the real
# actors' schedules, the stub agent, the stub gate and round VM, sim gh), then `sim replay`.
#
#   1. `sim run` exits 0, world up included. It is killed at RUN_LIMIT, a hang guard sized for a
#      loaded round VM, not a budget: at 60 s it went red under load in r-auto-93..97 while
#      passing alone (law-no-wall-clock-budgets-in-the-corpus).
#   2. The world ends with the bead LANDED on local/main, its publish PR merged and a release
#      tag in the forge.
#   3. `sim replay` of the same seed reports no divergence and leaves the events table
#      byte-identical.
#   4. CONTROL: a scenario whose goal cannot be reached (the bead is never filed) exits non-zero
#      naming the seed, so (1) is not a run that exits 0 whatever happens.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"

[ -n "${SPIRA_RELEASE:-}" ] || bail "SPIRA_RELEASE is not set: run via testenv --with-bins"
SIM="$SPIRA_RELEASE/bin/sim"
[ -x "$SIM" ] || bail "no sim in the staged release: $SIM"
command -v testenv >/dev/null || bail "testenv is not on PATH"

T="$(mktemp -d)"
trap 'rm -rf "$T"' EXIT
SEED=7
RUN_LIMIT=280
SCENARIO="$HERE/sim/scenarios/happy-path.toml"

REPO="$T/repo"
mkdir -p "$REPO"
tar -C "$HERE/.." --exclude=./target --exclude=./.git -cf - . | tar -xf - -C "$REPO"
git -C "$REPO" init -q --initial-branch=main && git -C "$REPO" add -A \
    && git -C "$REPO" -c user.name=t -c user.email=t@t.invalid commit -q -m seed || bail "fixture repo commit failed"

NOLOC=(-u SPIRA_RUN -u SPIRA_DB -u SPIRA_LC_PASSWORD_FILE -u SPIRA_LC_SOCKET -u SPIRA_LC_HOST -u SPIRA_LC_PORT -u SPIRA_LC_USER -u SPIRA_HOME -u SPIRA_WORK_BEAD_ID)

sim_in_repo() {  # sim_in_repo <seconds> <args...> — sim from the fixture repo, no production locator, a prebuilt release
    local limit="$1"; shift
    # batch-job: a whole simulated run, world up included; the caller names its wall-time limit
    (cd "$REPO" && timeout "$limit" env "${NOLOC[@]}" TMPDIR="$T" SPIRA_SIM_RELEASE="$SPIRA_RELEASE" SPIRA_IN_TESTENV=1 "$SIM" "$@")
}

dump() {  # dump — what the world did, for a red run (exec.log was copied out before the world went down)
    local w="$T"
    printf '%s\n' "${RUN_OUT:-}" | tail -4 | cut -c1-260 | sed 's/^/# run: /'
    [ -f "$w/exec.log" ] || return 0
    grep -c '^=== ' "$w/exec.log" | sed 's/^/# commands run: /'
    grep -A8 'pr=$(gh' "$w/exec.log" | grep -v 'probe' | grep '^===\|^sim\|^gh\|rror\|usage' | cut -c1-200 | tail -8 | sed 's/^/# step: /'
    grep '^=== ' "$w/exec.log" | sed -E 's/^=== t=([0-9]+) (.{0,44}).*(exit status: [0-9]+ in [0-9]+ms)$/\3 t=\1 \2/' | tail -14 | sed 's/^/# ran: /'
}

# --- 1. the happy path, timed ----------------------------------------------------------------
START=$SECONDS
out="$(sim_in_repo "$RUN_LIMIT" run happy-path --seed "$SEED" --keep 2>&1)"; rc=$?
echo "# sim run took $((SECONDS - START))s"
W="$(ls -d "$T"/sim-run-*-"$SEED" 2>/dev/null | head -1)"
wantrc "sim run happy-path exits 0 (killed at ${RUN_LIMIT}s if it hangs: rc 124)" 0 "$rc"
[ "$rc" = 0 ] || RUN_OUT="$out"
want "sim run prints the seed first" "sim seed: $SEED" "$out"

if [ -z "$W" ] || [ ! -d "$W" ]; then
    bail "no world was kept under $T"
fi

# --- 2. where the world ended -----------------------------------------------------------------
probe="$(sim_in_repo 30 probe "$W" 2>&1)"
want "the bead is LANDED" '"lc_state":"LANDED"' "$(printf '%s' "$probe" | tr -d ' ')"
want "its tip is on local/main" '"on_local_main":true' "$(printf '%s' "$probe" | tr -d ' ')"
is "the publish PR is merged" 1 "$(grep -c '"state": "MERGED"' "$W/gh/state.json")"
is "the forge holds a release tag" 1 "$(grep -c '"name": "spira-release-' "$W/gh/state.json")"
[ -s "$W/trace/events.jsonl" ] && ok "the run recorded events" || bad "the run recorded events" "no events.jsonl"

# --- 3. replay --------------------------------------------------------------------------------
cp "$W/trace/events.jsonl" "$T/events.first"
out="$(sim_in_repo 30 replay "$W" --seed "$SEED" 2>&1)"; rc=$?
wantrc "sim replay of the same seed reports no divergence" 0 "$rc"
[ "$rc" = 0 ] || printf '# replay: %s\n' "$out"
cmp -s "$T/events.first" "$W/trace/events.jsonl" && ok "replay leaves the events table byte-identical" || bad "replay leaves the events table byte-identical" "differs"

cp "$W/exec.log" "$T/exec.log" 2>/dev/null
sim_in_repo 60 world down "$W" >/dev/null 2>&1

# --- 4. CONTROL: an unreachable goal fails, naming the seed ------------------------------------
sed '/^\[\[step\]\]/,$d' "$SCENARIO" | sed 's/^horizon = .*/horizon = 300000/' > "$T/never.toml"
out="$(sim_in_repo 90 run "$T/never.toml" --seed 3 2>&1)"; rc=$?
wantrc "control: a scenario that never files its bead exits non-zero" 1 "$rc"
want "control: the failure names the unreached goal and the seed" "goal sp-hp01:LANDED unreached: seed 3" "$out"
for w in "$T"/sim-run-*-3; do [ -d "$w" ] && sim_in_repo 60 world down "$w" >/dev/null 2>&1; done

[ "$_TL_FAIL" -gt 0 ] && dump
tl_summary
