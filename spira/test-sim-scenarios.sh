#!/usr/bin/env bash
# tier: T2
# requires: testenv
# covers: spira/sim/** sim/actors.toml sim/durations.toml spira/release.sh .github/workflows/gate.yml lifecycle/src/** spira-lc/src/** work/src/** landing-pass/src/** queue/src/** batcher/src/** batcher-cut/src/** gate-worker/** systemd/spira-sentinel.* systemd/spira-rounds.* systemd/spira-publish.* systemd/spira-gate-worker.*
#
# test-sim-scenarios.sh — every scenario under spira/sim/scenarios/, run in a sim world by
# `sim run` under a fixed seed list plus one fresh seed, logged so a red reproduces.
#
#   1. Each (scenario, seed) exits 0: its goal is reached, every built-in and scenario
#      invariant holds, and every `expect` query found its row.
#   2. CONTROLS: a scenario whose agent moves the branch after submitting and never submits
#      again fails naming the tip invariant, and one with an expectation nothing can meet fails
#      naming it, so (1) is not a run that exits 0 whatever happens.
#
# Scenarios run side by side (SIM_SCENARIOS_PAR at a time) in their own worlds; the suite's
# wall time is the longest few worlds, not the sum.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"

[ -n "${SPIRA_RELEASE:-}" ] || bail "SPIRA_RELEASE is not set: run via testenv --with-bins"
SIM="$SPIRA_RELEASE/bin/sim"
[ -x "$SIM" ] || bail "no sim in the staged release: $SIM"
command -v testenv >/dev/null || bail "testenv is not on PATH"

T="$(mktemp -d)"
cleanup() {
    local w
    for w in "$T"/sim-run-*; do [ -d "$w" ] && timeout 60 "$SIM" world down "$w" >/dev/null 2>&1; done # batch-job: world teardown stops a whole scratch world
    rm -rf "$T"
}
trap cleanup EXIT
FIXED_SEEDS="${SIM_SCENARIOS_SEEDS:-7}"
FRESH_SEED=$(( $(date +%s%N) % 1000000000 ))
PAR="${SIM_SCENARIOS_PAR:-4}"
RUN_LIMIT=280
echo "# seeds: $FIXED_SEEDS and fresh $FRESH_SEED (SIM_SCENARIOS_SEEDS pins the fixed list)"

REPO="$T/repo"
mkdir -p "$REPO"
tar -C "$HERE/.." --exclude=./target --exclude=./.git -cf - . | tar -xf - -C "$REPO"
git -C "$REPO" init -q --initial-branch=main && git -C "$REPO" add -A \
    && git -C "$REPO" -c user.name=t -c user.email=t@t.invalid commit -q -m seed || bail "fixture repo commit failed"

NOLOC=(-u SPIRA_RUN -u SPIRA_DB -u SPIRA_LC_PASSWORD_FILE -u SPIRA_LC_SOCKET -u SPIRA_LC_HOST -u SPIRA_LC_PORT -u SPIRA_LC_USER -u SPIRA_HOME -u SPIRA_WORK_BEAD_ID)

run_one() {  # run_one <label> <scenario file or name> <seed> — result in $T/res.<label>.{out,rc,s}
    local label="$1" scenario="$2" seed="$3" start=$SECONDS rc
    # batch-job: a whole simulated run, world up included; RUN_LIMIT is its wall-time limit
    (cd "$REPO" && timeout "$RUN_LIMIT" nice -n 19 env "${NOLOC[@]}" TMPDIR="$T" SPIRA_SIM_RELEASE="$SPIRA_RELEASE" SPIRA_IN_TESTENV=1 "$SIM" run "$scenario" --seed "$seed" >"$T/res.$label.out" 2>&1)
    rc=$?
    echo "$rc" >"$T/res.$label.rc"
    echo "$((SECONDS - start))" >"$T/res.$label.s"
}

SCENARIOS=()
for f in "$HERE"/sim/scenarios/*.toml; do SCENARIOS+=("$(basename "$f" .toml)"); done
[ "${#SCENARIOS[@]}" -ge 9 ] || bail "fewer than nine scenarios under spira/sim/scenarios: ${SCENARIOS[*]}"

sed 's/"submit"\]$/"commit out\/a3.txt"]/' "$HERE/sim/scenarios/resubmit-moved-tip.toml" > "$T/stale-tip.toml"
grep -q 'a3.txt' "$T/stale-tip.toml" || bail "control fixture was not derived from the resubmit scenario"
{ cat "$HERE/sim/scenarios/resubmit-moved-tip.toml"; printf '\n[[expect]]\nname = "nothing_can_meet_this"\nsql = "SELECT 1 FROM bead_state WHERE bead = '"'"'sp-nobody'"'"'"\n'; } > "$T/unmet.toml"

JOBS=()
for s in "${SCENARIOS[@]}"; do
    for seed in $FIXED_SEEDS $FRESH_SEED; do JOBS+=("$s.$seed|$s|$seed"); done
done
JOBS+=("control-stale-tip|$T/stale-tip.toml|3" "control-unmet|$T/unmet.toml|3")

START=$SECONDS
for j in "${JOBS[@]}"; do
    IFS='|' read -r label scenario seed <<<"$j"
    while [ "$(jobs -rp | wc -l)" -ge "$PAR" ]; do wait -n; done
    run_one "$label" "$scenario" "$seed" &
done
wait
echo "# ${#JOBS[@]} runs took $((SECONDS - START))s"

dump() {  # dump <label> — what a red run said, and what its world did
    local label="$1" w
    grep -E '^sim' "$T/res.$label.out" | cut -c1-300 | head -5 | sed 's/^/# /'
    w="$(sed -n 's/^sim: world kept at //p' "$T/res.$label.out" | head -1)"
    [ -f "$w/trace/bead_state.jsonl" ] || return 0
    python3 - "$w" <<'PY' | sed 's/^/# /'
import json, sys
last = {}
for l in open(sys.argv[1] + '/trace/bead_state.jsonl'):
    d = json.loads(l)
    k = (d['lc_state'], (d['lc_tip'] or '')[:7] == (d['branch_tip'] or '')[:7], d['on_local_main'])
    if last.get(d['bead']) != k:
        print('seq', d['seq'], d['bead'], *k)
        last[d['bead']] = k
PY
    grep -E 'CHECK6|set aside|rror|refus' "$w/exec.log" | cut -c20-230 | tail -5 | sed 's/^/# log: /'
}

for j in "${JOBS[@]}"; do
    IFS='|' read -r label scenario seed <<<"$j"
    rc="$(cat "$T/res.$label.rc" 2>/dev/null || echo missing)"
    case "$label" in
    control-*) ;;
    *)
        wantrc "$label exits 0 in $(cat "$T/res.$label.s")s" 0 "$rc"
        [ "$rc" = 0 ] || dump "$label"
        ;;
    esac
done
want "control: a branch moved after submit trips the tip invariant" "invariant inv_tip_matches_branch violated: seed 3" "$(cat "$T/res.control-stale-tip.out")"
want "control: an expectation nothing meets is named" "expectation nothing_can_meet_this unmet: seed 3" "$(cat "$T/res.control-unmet.out")"
is "control: the stale-tip run exits 1" 1 "$(cat "$T/res.control-stale-tip.rc")"
is "control: the unmet run exits 1" 1 "$(cat "$T/res.control-unmet.rc")"

# ISOLATION: every world is down and nothing of it still runs (the corpus shares this box)
for w in "$T"/sim-run-*; do [ -d "$w" ] && timeout 60 "$SIM" world down "$w" >/dev/null 2>&1; done # batch-job: world teardown stops a whole scratch world
is "no world serve outlives its run" 0 "$(pgrep -fc "$T/sim-run-" || true)"

tl_summary
