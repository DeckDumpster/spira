#!/usr/bin/env bash
# tier: T2
# requires: testenv
# covers: spira/sim/**
# lane: sim
# pids: 400
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"
. "$HERE/sim-scenario-lib.sh"
sim="$SPIRA_RELEASE/bin/sim"
t="$(mktemp -d)"; repo="$t/repo"; sim_fixture_repo "$repo"
noloc=(-u SPIRA_RUN -u SPIRA_DB -u SPIRA_LC_PASSWORD_FILE -u SPIRA_LC_SOCKET -u SPIRA_LC_HOST -u SPIRA_LC_PORT -u SPIRA_LC_USER -u SPIRA_HOME -u SPIRA_WORK_BEAD_ID)
export SPIRA_SIM_CACHE=/tmp/sim-seed-cache
(cd "$repo" && env "${noloc[@]}" TMPDIR="$t" SPIRA_SIM_RELEASE="$SPIRA_RELEASE" SPIRA_IN_TESTENV=1 "$sim" world seed) >/dev/null 2>&1
(cd "$repo" && timeout 280 env "${noloc[@]}" TMPDIR="$t" SPIRA_SIM_RELEASE="$SPIRA_RELEASE" SPIRA_IN_TESTENV=1 "$sim" run "${SCN:-run-state-trail}" --seed 7 --keep) 2>&1 | tail -5
for w in "$t"/sim-run-*; do
  echo "== $w"; ls "$w/run" | head -60; ls "$w/run/queue" "$w/run/verdicts" "$w/run/ejected" 2>&1 | head -30
  for f in "$w"/trace/run_*.jsonl; do echo "-- $f"; tail -25 "$f" | cut -c1-300; done
  echo "-- landing.log"; tail -20 "$w/run/landing.log" "$w/run/landing-pass.log" 2>&1 | cut -c1-200
  timeout 60 "$sim" world down "$w" >/dev/null 2>&1
done
bad "dump" "see above"
tl_summary
