#!/usr/bin/env bash
# tier: T2
# requires: testenv
# covers: spira/sim/** sim/actors.toml sim/durations.toml systemd/spira-sentinel.* systemd/spira-rounds.* systemd/spira-publish.* systemd/spira-gate-worker.*
#
# test-sim-happy-path.sh — one bead walked from filed to LANDED, published and tagged, in a
# sim world: `sim run` of spira/sim/scenarios/happy-path.toml (file and claim steps, the real
# actors' schedules, the stub agent, the stub gate and round VM, sim gh), then `sim replay`.
#
#   1. `sim run` exits 0, world up included, inside 60 s of wall time (the run is killed at that
#      limit, so a slow run is a failed exit, not a measured number compared afterwards).
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
RUN_LIMIT=60
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
out="$(sim_in_repo "$RUN_LIMIT" run "$SCENARIO" --seed "$SEED" --keep 2>&1)"; rc=$?
echo "# sim run took $((SECONDS - START))s"
W="$(ls -d "$T"/sim-run-*-"$SEED" 2>/dev/null | head -1)"
wantrc "sim run happy-path exits 0 within ${RUN_LIMIT}s (124 is the limit)" 0 "$rc"
[ "$rc" = 0 ] || RUN_OUT="$out"
want "sim run prints the seed first" "sim seed: $SEED" "$out"

if [ -z "$W" ] || [ ! -d "$W" ]; then
    bail "no world was kept under $T"
fi


in_world() {
    local -a kv=(); local line
    while IFS= read -r line; do [ -n "$line" ] && kv+=("$line"); done < "$W/config/sim.env"
    (cd "$W/work" && env "${NOLOC[@]}" SPIRA_TOML="$W/config/sim.toml" "${kv[@]}" PATH="$W/bin:$W/release/bin:$W/release/spira:$PATH" "$@")
}
tm() { local s=$(date +%s%N); in_world "$@" >/dev/null 2>&1; echo "# $(( ($(date +%s%N) - s) / 1000000 ))ms: $*" | cut -c1-120; }
echo "# tools: $(command -v strace perf ltrace | tr '\n' ' ')"
in_world landing-pass land 2>&1 | while IFS= read -r l; do printf '%s %s\n' "$(date +%s.%N)" "$l"; done | awk '{t=$1; $1=""; if (NR>1 && t-p>0.4) printf "# +%.1fs after: %s\n#      then: %s\n", t-p, substr(prev,1,150), substr($0,1,150); p=t; prev=$0} END{print "# total lines " NR}'
bad "prof" "forced"
tl_summary
