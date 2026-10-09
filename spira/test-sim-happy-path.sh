#!/usr/bin/env bash
# tier: T2
# requires: testenv
# covers: spira/sim/** sim/actors.toml sim/durations.toml systemd/spira-sentinel.* systemd/spira-rounds.* systemd/spira-publish.* systemd/spira-gate-worker.*
#
# test-sim-happy-path.sh — one bead walked from filed to LANDED inside a sim world.
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

REPO="$T/repo"
mkdir -p "$REPO"
tar -C "$HERE/.." --exclude=./target --exclude=./.git -cf - . | tar -xf - -C "$REPO"
git -C "$REPO" init -q --initial-branch=main && git -C "$REPO" add -A \
    && git -C "$REPO" -c user.name=t -c user.email=t@t.invalid commit -q -m seed || bail "fixture repo commit failed"

NOLOC=(-u SPIRA_RUN -u SPIRA_DB -u SPIRA_LC_PASSWORD_FILE -u SPIRA_LC_SOCKET -u SPIRA_LC_HOST -u SPIRA_LC_PORT -u SPIRA_LC_USER -u SPIRA_HOME -u SPIRA_WORK_BEAD_ID)
sim() { (cd "$REPO" && env "${NOLOC[@]}" SPIRA_SIM_RELEASE="$SPIRA_RELEASE" SPIRA_IN_TESTENV=1 "$SIM" "$@"); }

START=$SECONDS
out="$(timeout 120 env "${NOLOC[@]}" TMPDIR="$T" SPIRA_SIM_RELEASE="$SPIRA_RELEASE" SPIRA_IN_TESTENV=1 bash -c "cd '$REPO' && '$SIM' run '$HERE/sim/scenarios/happy-path.toml' --seed $SEED --keep" 2>&1)"; rc=$?
ELAPSED=$((SECONDS - START))
W="$(printf '%s\n' "$out" | sed -n 's/^sim: world kept at //p' | tail -1)"
wantrc "sim run happy-path exits 0" 0 "$rc"
if [ "$rc" != 0 ]; then
    printf '%s\n' "$out" | tail -15 | cut -c1-300 | sed 's/^/# run: /'
    if [ -f "$W/exec.log" ]; then
        grep -c '^===' "$W/exec.log" | sed 's/^/# commands run: /'
        grep '^=== ' "$W/exec.log" | grep -v ': exit status: 0' | tail -8 | cut -c1-200 | sed 's/^/# nonzero: /'
        tail -12 "$W/exec.log" | cut -c1-300 | sed 's/^/# log: /'
    fi
fi
[ "$ELAPSED" -le 60 ] && ok "the run took ${ELAPSED}s (<= 60s)" || bad "the run took ${ELAPSED}s (<= 60s)" "${ELAPSED}s"

if [ -f "$W/trace/events.jsonl" ]; then
    cp "$W/trace/events.jsonl" "$T/events.first"
    out="$(sim replay "$W" --seed "$SEED" 2>&1)"; rc=$?
    wantrc "sim replay of the same seed reports no divergence" 0 "$rc"
    [ "$rc" = 0 ] || printf '# replay: %s\n' "$out"
    cmp -s "$T/events.first" "$W/trace/events.jsonl" && ok "replay leaves the events table byte-identical" || bad "replay leaves the events table byte-identical" "differs"
fi

if [ -d "$W" ]; then sim world down "$W" >/dev/null 2>&1; fi
tl_summary
