#!/usr/bin/env bash
#
# test-sentinel-capacity.sh — a zero-capacity pass (SPIRA_MAX_AEONS=0) produces a
#   log message distinguishable from a pass that could not read the database;
#   neither reports 'goal reached' when nothing was attempted.
#
#   ./test-sentinel-capacity.sh
#
# WHAT THIS TESTS. When bd cannot reach the database, every state read — open_children,
# plan_ready, plan_inprog — returns 0 or empty, so before sp-4fss both a broken database
# and a finished goal produced the same log line: "pass complete — 0 action(s), 0
# progress, goal reached". The test instance runs at SPIRA_MAX_AEONS=0 permanently,
# which means a broken sentinel is indistinguishable from a working one unless the two
# failure modes produce different output.
#
# ACCEPTANCE (verbatim from sp-4fss):
#   a pass at MAX_AEONS=0 is distinguishable in the log from a pass that could not
#   read the graph; neither reports 'goal reached' when nothing was attempted.
#
# TWO CASES, BOTH SIDES EXERCISED:
#   1. POSITIVE CONTROL (DB readable, MAX_AEONS=0, goal reached) — sentinel exits 0,
#      logs "goal reached". Without this, a check that always exits 1 reads as correct.
#   2. DB UNREADABLE — sentinel exits 1, logs "DATABASE UNREADABLE", never "goal reached".
#
# RUN STRATEGY. sentinel.sh is invoked as a subprocess with a minimal environment:
# no real systemd, no real gh, no real network. External scripts (pilgrimage.sh,
# strand.sh, etc.) are stubs that exit 0 and print nothing. The fixture DB carries only
# the goal epic with no open children, so the pass sees 0 open beads and GOAL_REACHED=1.
#
# A REAL bd ON A FIXTURE DATABASE (law-prefer-the-real-dependency). The DB check this
# suite covers calls bdq (real bd); a stub would prove nothing about whether bd actually
# fails on a bad path.
#
# defect: sp-4fss
# covers: spira/sentinel.sh spira/lib.sh
# hermetic-ok: uses a fixture database; systemd/gh/network reached through SPIRA_LAUNCH,
#   SPIRA_SUMMON and SPIRA_SYSTEMCTL seams which are pointed at stubs
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()   { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()  { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want() { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
lack() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }
is()   { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-sentinel-capacity
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up sentinel_capacity || { echo "test-sentinel-capacity: could not build fixture database"; exit 1; }

# Seed the fixture with a goal epic that has no open children. The sentinel sees
# n_open=0 and sets GOAL_REACHED=1 — which is the state we want for the positive control.
testdb_reset
testdb_seed <<'JSONL'
{"id":"sp-epic1","title":"test goal epic","status":"open","issue_type":"epic","labels":["plan","spira"]}
JSONL
GOAL_DB="$SPIRA_DB"

# STUBS directory: scripts sentinel.sh calls via $SPIRA_HOME.
# Each one exits 0 and produces no output so no CHECK fires an action from them.
STUBS="$TMP/stubs"
mkdir -p "$STUBS/chamber"     # empty chamber → no fayths from filesystem

for _name in pilgrimage.sh strand.sh sending.sh reflect.sh; do
    printf '#!/bin/sh\n# hermetic-ok: stub for test-sentinel-capacity\n' \
        > "$STUBS/$_name"
    chmod +x "$STUBS/$_name"
done
# mock-systemctl always says the service is inactive — land_active → false, so the
# landing dispatch path is taken rather than the "already in flight" branch.
printf '#!/bin/sh\n# hermetic-ok: stub for test-sentinel-capacity\necho inactive\n' \
    > "$STUBS/mock-systemctl"
chmod +x "$STUBS/mock-systemctl"
for _name in mock-launch mock-summon mock-notify; do
    printf '#!/bin/sh\n# hermetic-ok: stub for test-sentinel-capacity\nexit 0\n' \
        > "$STUBS/$_name"
    chmod +x "$STUBS/$_name"
done
# Empty repo-map so repo_root returns failure for any name — the partition loops
# do not run (SPIRA_FAYTHS="") so CHECK 5 / CHECK 6 never reach repo_root.
touch "$STUBS/repo-map"

# run_sentinel <db-path> <MAX_AEONS> → combined stdout+stderr; exits with sentinel's code.
#
# An explicit minimal environment (law-gates-run-in-a-clean-environment). SPIRA_PATH and
# SPIRA_BD are inherited from testdb_up so that conf.sh's PATH reset does not lose the
# embedded bd binary that the fixture requires.
_run_cnt=0
run_sentinel() {
    local db="$1" max_aeons="$2"
    _run_cnt=$((_run_cnt + 1))
    local run="$TMP/run-$_run_cnt"; mkdir -p "$run"
    env -i \
        PATH="$PATH" HOME="$HOME" \
        SPIRA_HOME="$STUBS" \
        SPIRA_RUN="$run" \
        SPIRA_DB="$db" \
        SPIRA_BD="$SPIRA_BD" \
        SPIRA_PATH="$SPIRA_PATH" \
        SPIRA_GOAL="sp-epic1" \
        SPIRA_FAYTHS="" \
        SPIRA_MAX_AEONS="$max_aeons" \
        SPIRA_LAND_STALE=999999 \
        SPIRA_SYSTEMCTL="$STUBS/mock-systemctl" \
        SPIRA_LAUNCH="$STUBS/mock-launch" \
        SPIRA_SUMMON="$STUBS/mock-summon" \
        SPIRA_NOTIFY="$STUBS/mock-notify" \
        bash "$HERE/sentinel.sh" 2>&1
}

echo "test-sentinel-capacity.sh"

# ======================================================================================
echo
echo "positive control — DB readable, MAX_AEONS=0, goal reached:"
# ======================================================================================
# The goal epic has no open children, the database is readable, and the pool is zero.
# The sentinel must complete the pass and log "goal reached". Without this half, a check
# that always exits 1 would pass both assertions below for the wrong reason.
out="$(run_sentinel "$GOAL_DB" 0)"; rc=$?
is   "exits 0 when the database is readable"          "0" "$rc"
want "reports 'goal reached' (goal confirmed by DB)"  "goal reached" "$out"
lack "does NOT report DATABASE UNREADABLE"            "DATABASE UNREADABLE" "$out"

# ======================================================================================
echo
echo "DB unreadable — sentinel exits 1 with a distinguishable message:"
# ======================================================================================
# When bd cannot reach the database, every state read returns 0 or empty, so before
# sp-4fss both a broken database and a finished goal produced "goal reached". The fix
# exits 1 with "DATABASE UNREADABLE" before any state is examined.
#
# A non-existent path is the simplest case: bd -C /no-such-db fails unconditionally.
out="$(run_sentinel "$TMP/no-such-db" 0)"; rc=$?
is   "exits 1 when the database is unreadable"        "1" "$rc"
want "reports DATABASE UNREADABLE"                    "DATABASE UNREADABLE" "$out"
lack "does NOT report 'goal reached'"                 "goal reached" "$out"

# DISTINGUISHABILITY: these two cases now produce different exit codes AND different
# log messages. The DB-unreadable case (exit 1, "DATABASE UNREADABLE") is distinguished
# from the zero-capacity pass (exit 0, "goal reached") on both axes.

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
