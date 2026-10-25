# sim-scenario-lib.sh — Sourced, never executed. One scenario file run in a sim world per seed, shared by the
# test-sim-<scenario>.sh shards. Sourced after testlib.sh; defines sim_scenario_shard and
# sim_fixture_repo.

sim_fixture_repo() {  # sim_fixture_repo <dir> — the harness tree as a one-commit repo
    local repo="$1" here
    here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
    mkdir -p "$repo"
    tar -C "$here/.." --exclude=./target --exclude=./.git -cf - . | tar -xf - -C "$repo"
    git -C "$repo" init -q --initial-branch=main && git -C "$repo" add -A \
        && git -C "$repo" -c user.name=t -c user.email=t@t.invalid commit -q -m seed || bail "fixture repo commit failed"
}

sim_failed_commands() {  # sim_failed_commands <run output> — the output of every kept-world command that exited neither 0 nor 3
    local w
    w="$(sed -n 's/^sim: world kept at //p' "$1" | head -1)"
    [ -f "$w/exec.log" ] || return 0
    awk '/^=== t=/ { show = ($0 ~ /exit status: [1-9]/ && $0 !~ /exit status: 3 /) } show' "$w/exec.log" | cut -c1-300 | head -40
}

sim_scenario_shard() {  # sim_scenario_shard <scenario name>
    local name="$1" here seeds fresh="" sim t repo seed rc start label
    here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
    [ -n "${SPIRA_RELEASE:-}" ] || bail "SPIRA_RELEASE is not set: run via testenv --with-bins"
    sim="$SPIRA_RELEASE/bin/sim"
    [ -x "$sim" ] || bail "no sim in the staged release: $sim"
    [ -f "$here/sim/scenarios/$name.toml" ] || bail "no scenario $name under spira/sim/scenarios"
    t="$(mktemp -d)"
    # shellcheck disable=SC2064
    trap "for w in '$t'/sim-run-*; do [ -d \"\$w\" ] && timeout 60 '$sim' world down \"\$w\" >/dev/null 2>&1; done; rm -rf '$t'" EXIT # batch-job: world teardown stops a whole scratch world
    seeds="${SIM_SCENARIOS_SEEDS:-7}"
    [ "${SIM_SCENARIOS_FRESH:-0}" = 1 ] && fresh=$(( $(date +%s%N) % 1000000000 ))
    echo "# seeds: $seeds${fresh:+ and fresh $fresh}"
    repo="$t/repo"
    sim_fixture_repo "$repo"
    local noloc=(-u SPIRA_RUN -u SPIRA_DB -u SPIRA_LC_PASSWORD_FILE -u SPIRA_LC_SOCKET -u SPIRA_LC_HOST -u SPIRA_LC_PORT -u SPIRA_LC_USER -u SPIRA_HOME -u SPIRA_WORK_BEAD_ID)
    # The seed repo of this tree is built once for every world of the container, here or in a sibling shard.
    export SPIRA_SIM_CACHE="${SPIRA_SIM_CACHE:-/tmp/sim-seed-cache}"
    start=$SECONDS
    # batch-job: the seed repo every world copies
    (cd "$repo" && timeout 280 env "${noloc[@]}" TMPDIR="$t" SPIRA_SIM_RELEASE="$SPIRA_RELEASE" SPIRA_IN_TESTENV=1 "$sim" world seed) >"$t/seed.out" 2>&1 || bail "sim world seed failed: $(cat "$t/seed.out")"
    echo "# seed repo ready in $((SECONDS - start))s (the worlds copy it)"
    for seed in $seeds $fresh; do
        label="$name.$seed"
        start=$SECONDS
        # batch-job: a whole simulated run, world up included; 280 s is a hang guard, not a budget
        (cd "$repo" && timeout 280 env "${noloc[@]}" TMPDIR="$t" SPIRA_SIM_RELEASE="$SPIRA_RELEASE" SPIRA_IN_TESTENV=1 "$sim" run "$name" --seed "$seed" >"$t/res.out" 2>&1)
        rc=$?
        wantrc "$label exits 0 in $((SECONDS - start))s" 0 "$rc"
        grep -h '^sim: profile' "$t/res.out" | sed "s/^/# $label /"
        [ "$rc" = 0 ] || grep -E '^sim' "$t/res.out" | cut -c1-300 | head -5 | sed 's/^/# /'
        [ "$rc" = 0 ] || sim_failed_commands "$t/res.out" | sed "s/^/# $label /"
        [ "$rc" = 0 ] || { w="$(sed -n 's/^sim: world kept at //p' "$t/res.out" | head -1)"; awk '/^=== t=.*sentinel --summon-only/ { n++ } n >= 1 && n <= 2' "$w/exec.log" | cut -c1-300 | head -60 | sed "s/^/# DBG /"; tail -30 "$w/run/sentinel.log" 2>/dev/null | cut -c1-300 | sed "s/^/# DBGLOG /"; }
    done
    for w in "$t"/sim-run-*; do [ -d "$w" ] && timeout 60 "$sim" world down "$w" >/dev/null 2>&1; done # batch-job: world teardown stops a whole scratch world
    is "no world serve outlives its run" 0 "$(pgrep -fc "$t/sim-run-" || true)"
    tl_summary
}
