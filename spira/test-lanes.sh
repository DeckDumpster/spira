#!/usr/bin/env bash
#
# test-lanes.sh — priority lanes: not blocked by a saturated pool; escape hatch works
# when the scheduler's ordering logic is broken.
#
#   ./test-lanes.sh
#
# THREE ACCEPTANCE CRITERIA (bead sp-vyl4):
#   1. Pool saturated — a lane-tagged bead is still claimed (the lane has its own capacity)
#   2. Broken ordering — the escape hatch reaches a bead the normal scheduler skips
#   3. Ops guarantee — ops.fayth now declares FAYTH_LANE=ops and is excluded from the pool
#
# WHAT A LANE IS, in two sentences. A lane fayth declares FAYTH_LANE=<name> and draws from
# its own FAYTH_MAX_CONCURRENT, never from SPIRA_MAX_AEONS. The sentinel handles it in a
# separate loop after the pool, without a pool argument, so a fully-occupied builder pool
# can never block a lane fayth.
#
# THE ESCAPE HATCH is escape.sh: it invokes aeon.sh directly, bypassing the sentinel's
# pool and lane capacity checks. The test verifies that when pool=0 and summon_fayth
# returns "nothing to summon", escape.sh still fires. This is the mechanism for the case
# where the scheduler itself is broken.
#
# POSITIVE CONTROLS. Every case that asserts absence first proves the same path produces
# presence, so a check pointed at the wrong thing and a check that found nothing look
# different (law-absence-needs-a-positive-control).
#
# defect: sp-vyl4
# covers: spira/lib.sh spira/conf.sh spira/sentinel.sh spira/escape.sh spira/chamber/ops.fayth
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"

pass=0; fail=0
ok()    { pass=$((pass+1)); printf '  ok   — %s\n' "$1"; }
bad()   { fail=$((fail+1)); printf '  FAIL — %s: %s\n' "$1" "${2:-}"; }
is()    { [ "$2" = "$3" ] && ok "$1" || bad "$1" "expected [$2] got [$3]"; }
want()  { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant(){ [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT INT TERM
mkdir -p "$T/run" "$T/chamber" "$T/bin"

# A MINIMAL ENVIRONMENT, non-default everywhere so that a test asserting the default does
# not pass by having the literal written into the code (law-gates-run-in-a-clean-environment).
export SPIRA_RUN="$T/run"
export SPIRA_CONF="$T/no-such.conf"
export SPIRA_HOME="$T"
export SPIRA_DB="$T/no-db"   # no database needed for the library-level tests

. "$HERE/lib.sh"

# STUB OVERRIDES so the tests never touch a real database, a real process table, or
# real systemd — and so the controllable cases are not confused with ambient state.
MOCK_READY=0
fayth_ready() { printf '%d' "$MOCK_READY"; }
MOCK_COUNT=0
aeon_count()  { printf '%d' "$MOCK_COUNT"; }
capacity_paused() { return 1; }  # no outage

# A TASK FAYTH and a LANE FAYTH, neither named "builder" or "ops" — the rule must be
# bound to the declaration, not to a hardcoded name.
cat > "$T/chamber/worker.fayth" <<'F'
FAYTH_NAME=worker
FAYTH_LABELS="spira,plan"
FAYTH_EXCLUDE_LABELS="spira-poison"
FAYTH_MAX_CONCURRENT=2
FAYTH_ELASTIC=1
FAYTH_HEARTBEAT_SECONDS=120
F

cat > "$T/chamber/guardian.fayth" <<'F'
FAYTH_NAME=guardian
FAYTH_LABELS="spira,incident"
FAYTH_EXCLUDE_LABELS="spira-poison"
FAYTH_MAX_CONCURRENT=1
FAYTH_HEARTBEAT_SECONDS=60
FAYTH_LANE=priority
F

# ==========================================================================================
echo
echo "spira_lane_fayths / spira_task_fayths — the roster split"
# ==========================================================================================
export SPIRA_FAYTHS="worker guardian"

lane="$(spira_lane_fayths)"
task="$(spira_task_fayths)"

is "spira_lane_fayths returns the FAYTH_LANE fayth"      "guardian" "$lane"
is "spira_task_fayths excludes the FAYTH_LANE fayth"     "worker"   "$task"
nowant "the lane fayth does not appear in task fayths"   "guardian" "$task"
nowant "the task fayth does not appear in lane fayths"   "worker"   "$lane"

# POSITIVE CONTROL: both functions return something, so absence above is the exclusion
# working and not both functions returning empty.
is "there is at least one lane fayth"  "1" "$([ -n "$lane" ] && echo 1 || echo 0)"
is "there is at least one task fayth"  "1" "$([ -n "$task" ] && echo 1 || echo 0)"

# ==========================================================================================
echo
echo "criterion 1 — lane fayth is summonable when task pool is saturated"
# ==========================================================================================
# A pool of 0 means the task pool is exhausted. summon_fayth for a task fayth with pool=0
# returns 1 (nothing to start). summon_fayth for a lane fayth with NO pool argument returns
# based only on FAYTH_MAX_CONCURRENT — the pool never clamps it.
#
# The POSITIVE CONTROL comes first: with pool=1, the task fayth CAN be summoned.

SUMMONED="$T/run/summoned.log"
# Mock the summon so no real systemd-run is called. Record the fayth name.
summon_mock() {
    printf '%s\n' "SUMMONED:$1" >> "$SUMMONED"
    return 0
}
# Override summon_fayth's call to the real summon binary via SPIRA_SUMMON:
export SPIRA_SUMMON="$T/bin/mock-summon"
cat > "$T/bin/mock-summon" <<'MOCK'
#!/usr/bin/env bash
# Pull the fayth name out of the aeon.sh <fayth> argument at the end of the line.
# The call is: mock-summon --user --collect ... aeon.sh <fayth> [--dry-run]
fayth="${@: -1}"      # last argument is --dry-run OR the fayth name
[ "$fayth" = "--dry-run" ] && fayth="${@: -2:1}"
printf 'SUMMONED:%s\n' "$fayth" >> "$SUMMONED_FILE"
exit 0
MOCK
chmod +x "$T/bin/mock-summon"
export SUMMONED_FILE="$SUMMONED"

MOCK_READY=1
MOCK_COUNT=0

# Positive control: task fayth with pool=1 is summoned
rm -f "$SUMMONED"
summon_fayth worker 1
is "positive: task fayth with pool=1 is summoned" "SUMMONED:worker" "$(cat "$SUMMONED" 2>/dev/null)"

# Pool saturated (pool=0): task fayth is NOT summoned
rm -f "$SUMMONED"
summon_fayth worker 0 || true
is "task fayth with pool=0 is not summoned" "absent" \
   "$( [ -f "$SUMMONED" ] && cat "$SUMMONED" || echo absent )"

# Pool saturated but lane fayth IS summoned (no pool arg)
rm -f "$SUMMONED"
summon_fayth guardian    # no pool argument — the lane path
is "lane fayth without pool arg IS summoned even when pool would be 0" \
   "SUMMONED:guardian" "$(cat "$SUMMONED" 2>/dev/null)"

# fayth_free confirms the arithmetic: with pool=0 a task/elastic fayth gets 0 free;
# a non-elastic lane fayth without pool gets (cap - running).
is "task elastic fayth: fayth_free returns 0 when pool=0"    "0" "$(fayth_free worker 0)"
is "task elastic fayth: fayth_free returns 2 when pool=2"    "2" "$(fayth_free worker 2)"
is "lane fayth: fayth_free without pool returns cap-running" "1" "$(fayth_free guardian)"

# With running=1 (at cap), lane fayth also cannot start more
MOCK_COUNT=1
is "lane fayth at cap returns 0 free"    "0" "$(fayth_free guardian)"
MOCK_COUNT=0

# ==========================================================================================
echo
echo "criterion 2 — escape hatch reaches a bead when ordering logic is broken"
# ==========================================================================================
# "Ordering logic broken" means: summon_fayth returns 1 for a fayth that has ready work.
# This happens when pool=0 (pool logic broken/saturated) or when a fayth is incorrectly
# excluded. escape.sh bypasses those checks and invokes the aeon directly.
#
# The POSITIVE CONTROL: summon_fayth("worker", 0) returns 1 (nothing summoned). Then
# escape.sh succeeds for the same fayth, proving the escape bypasses the pool check.
#
# SUBPROCESS STUB. Shell function overrides (fayth_ready, capacity_paused) do not propagate
# into `bash escape.sh` — a subshell sources lib.sh fresh. SPIRA_BD is exported to a shim
# that answers "ready" queries from a sentinel file, so the test can toggle readiness for
# the subprocess without needing a real Dolt database.
export FAKE_READY_FILE="$T/run/fake-ready"
cat > "$T/bin/fake-bd" <<'FAKEBD'
#!/usr/bin/env bash
for arg; do
    if [ "$arg" = "ready" ]; then
        [ -f "${FAKE_READY_FILE:-}" ] && printf '[{"id":"sp-test"}]\n' || printf '[]\n'
        exit 0
    fi
done
exit 0
FAKEBD
chmod +x "$T/bin/fake-bd"
export SPIRA_BD="$T/bin/fake-bd"

MOCK_READY=1
touch "$T/run/fake-ready"     # subprocess sees: 1 ready bead

# Prove normal summon FAILS for the worker fayth when pool=0
rm -f "$SUMMONED"
summon_fayth worker 0 || true
normal_result="$( [ -f "$SUMMONED" ] && cat "$SUMMONED" || echo absent )"
is "positive: normal summon with pool=0 produces nothing" "absent" "$normal_result"

# escape.sh bypasses the pool and still summons
rm -f "$SUMMONED"
# escape.sh sources lib.sh and calls the same SPIRA_SUMMON
bash "$HERE/escape.sh" worker 2>/dev/null || true
escape_result="$(cat "$SUMMONED" 2>/dev/null)"
want "escape.sh summons the worker despite pool=0 not being passed" "SUMMONED:worker" "$escape_result"

# THE CONTROL THAT MAKES THE ABOVE MEANINGFUL: escape.sh with nothing ready exits 0 but
# does not summon — so the summon above is about the fayth having work, not about the
# script always calling the binary unconditionally.
MOCK_READY=0
rm -f "$T/run/fake-ready"     # subprocess sees: 0 ready beads
rm -f "$SUMMONED"
bash "$HERE/escape.sh" worker 2>/dev/null || true
is "escape.sh with nothing ready does not summon" "absent" \
   "$( [ -f "$SUMMONED" ] && cat "$SUMMONED" || echo absent )"
MOCK_READY=1
touch "$T/run/fake-ready"

# ==========================================================================================
echo
echo "criterion 3 — ops.fayth declares FAYTH_LANE=ops and is excluded from the task pool"
# ==========================================================================================
# ops.fayth shipped with FAYTH_ROLE=party. The new mechanism is FAYTH_LANE=ops. This test
# proves the shipped fayth file carries the new declaration, that the shipped behaviour
# (not drawn from the task pool) is preserved, and that the ops lane is declared in conf.sh.
#
# Test at the file level (assertions about what is WRITTEN) so the result is authoritative
# and not contingent on which environment the suite runs in.

want "ops.fayth declares FAYTH_LANE=ops" "FAYTH_LANE=ops" "$(cat "$HERE/chamber/ops.fayth")"
nowant "ops.fayth no longer uses FAYTH_ROLE=party" "FAYTH_ROLE=party" "$(grep -v '^#' "$HERE/chamber/ops.fayth")"

# With the real fayth roster, ops is in lane fayths and NOT in task fayths.
export SPIRA_FAYTHS="builder ops"
export SPIRA_HOME="$HERE"      # point at the real chamber

real_task="$(spira_task_fayths)"
real_lane="$(spira_lane_fayths)"

want   "ops appears in lane fayths" "ops" "$real_lane"
nowant "ops does NOT appear in task fayths" "ops" "$real_task"
want   "builder is still in the task pool" "builder" "$real_task"

# The ops lane is declared in conf.sh's key list (SPIRA_CONF_KEYS) and defaults.
want "SPIRA_LANES is a recognised conf key" "SPIRA_LANES" "$(cat "$HERE/conf.sh")"
want "ops is in the SPIRA_LANES default"    "ops"          "$(grep 'SPIRA_LANES:=' "$HERE/conf.sh")"

# The sentinel handles lane fayths in a separate loop.
want "sentinel.sh references spira_lane_fayths" "spira_lane_fayths" "$(cat "$HERE/sentinel.sh")"
want "sentinel.sh handles LANE_FAYTHS in CHECK 7" "LANE_FAYTHS" "$(cat "$HERE/sentinel.sh")"

# escape.sh exists and is executable.
is "escape.sh is executable" "0" "$([ -x "$HERE/escape.sh" ] && echo 0 || echo 1)"
want "escape.sh references the lane escape rationale" "control plane" "$(cat "$HERE/escape.sh")"

echo
printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
