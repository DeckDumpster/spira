#!/usr/bin/env bash
#
# test-sending-metrics.sh — the Sending's counters count BRANCHES, and one event is counted once.
#
#   ./test-sending-metrics.sh
#
# WHAT THIS SUITE IS FOR
# ----------------------
# The ops pane rendered "184 fiends — unsent work that came back" while the true number was
# TWO: sp-gate-rebuild, which git refused to delete on 89 consecutive passes, and
# sp-supersede-key on 4. The operator read it beside "3 awaiting rites" and reasonably
# concluded there was a large backlog of dead work. There was none.
#
# Two defects produced that number, and this suite exists because neither was detectable from
# the outside — both render as a plausible integer, and a plausible integer is exactly what a
# broken counter looks like (law-alerts-must-be-actionable: a false alert spends attention,
# and the next true one is not believed).
#
#   1. LINES, NOT BRANCHES. The sending runs every two minutes, so one branch it cannot
#      delete scores ~720 a day. The identical bug was found and fixed for held/kept — "the
#      pane read 'held 221' for 23 distinct branches" — and sent/failed were left as bare
#      counters directly beneath the comment recording it.
#
#   2. ONE EVENT COUNTED TWICE. The predicate matched both sending.sh's per-branch
#      `FAILED <id>` and sentinel.sh's per-pass summary "sending reported a branch it could
#      not delete", which is emitted only when the former is already present in the same
#      captured output. 93 refusals rendered as 186.
#
# THE PROPERTY UNDER TEST IS A DISTINCTION, NOT A VALUE. A suite asserting only "failed == 2"
# on a fixture with two failures passes just as well against a line-counter if the fixture
# happens to have one line each. So every fixture here repeats its events across many passes,
# which is the shape the real log has and the only shape that can tell the two implementations
# apart. That is law-absence-needs-a-positive-control in its positive form: prove the counter
# CAN see a second distinct branch, in the same fixture where it must not see a second copy of
# the first.
#
# NO DATABASE, NO BOX. sending_metrics is a pure function over log lines, so the fixture is a
# string and the suite is hermetic by construction.
#
# defect: sp-1smg
# covers: spira/cockpit-metrics.py
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

echo "test-sending-metrics.sh"

# env -i so no real spira.conf can reach the module (law-gates-run-in-a-clean-environment).
run_fixture() {   # run_fixture <fixture-on-stdin> -> "SENT HELD KEPT FAILED"
    env -i PATH="/usr/bin:/bin" PYTHONPATH="$HERE" HOME="$HOME" \
        python3 -c '
import sys, datetime, importlib.util
spec = importlib.util.spec_from_file_location("cm", sys.argv[1])
cm = importlib.util.module_from_spec(spec); spec.loader.exec_module(cm)
lines = sys.stdin.read().splitlines()
since = datetime.datetime(2000, 1, 1, tzinfo=datetime.timezone.utc)
d = cm.sending_metrics(lines, since)
print(d["SP_SENT"], d["SP_SENT_HELD"], d["SP_SENT_KEPT"], d["SP_SENT_FAILED"])
' "$HERE/cockpit-metrics.py"
}

# run_fixture_age: same as run_fixture but prints FAILED_AGE_M too.
# now is pinned to 2026-09-08T12:00:00Z so age assertions are deterministic.
run_fixture_age() {  # run_fixture_age <fixture-on-stdin> -> "FAILED FAILED_AGE_M"
    env -i PATH="/usr/bin:/bin" PYTHONPATH="$HERE" HOME="$HOME" \
        python3 -c '
import sys, datetime, importlib.util
spec = importlib.util.spec_from_file_location("cm", sys.argv[1])
cm = importlib.util.module_from_spec(spec); spec.loader.exec_module(cm)
lines = sys.stdin.read().splitlines()
since = datetime.datetime(2000, 1, 1, tzinfo=datetime.timezone.utc)
now   = datetime.datetime(2026, 9, 8, 12, 0, 0, tzinfo=datetime.timezone.utc)
d = cm.sending_metrics(lines, since, now)
print(d["SP_SENT_FAILED"], d["SP_SENT_FAILED_AGE_M"])
' "$HERE/cockpit-metrics.py"
}

# ---------------------------------------------------------------------------------------
# THE REAL SHAPE: one branch git will not delete, refused on pass after pass. This is
# sp-gate-rebuild's actual history in miniature. A line-counter says 5; the truth is 1.
# sentinel.sh's summary line rides along on every pass, as it does in the real log.
# ---------------------------------------------------------------------------------------
fixture_one_stuck() {
    for i in 1 2 3 4 5; do
        printf '2026-09-07T0%d:00:00Z spira: state: pass\n' "$i"
        printf 'FAILED sp-gate-rebuild  branch and worktree\n'
        printf '2026-09-07T0%d:00:01Z spira: sending reported a branch it could not delete\n' "$i"
    done
}
read -r s h k f <<< "$(fixture_one_stuck | run_fixture)"
is "one branch refused on five passes counts as ONE fiend" "1" "$f"

# ---------------------------------------------------------------------------------------
# THE POSITIVE CONTROL. Same fixture, plus a SECOND distinct branch. If the count were
# clamped, deduped wrongly, or simply hard-coded, this is where it shows: the suite must
# prove the counter can still see a new branch while refusing to see a repeat of the old one.
# ---------------------------------------------------------------------------------------
fixture_two_stuck() {
    fixture_one_stuck
    printf '2026-09-07T09:00:00Z spira: state: pass\n'
    printf 'FAILED sp-supersede-key  branch and worktree\n'
    printf '2026-09-07T09:00:01Z spira: sending reported a branch it could not delete\n'
    printf '2026-09-07T10:00:00Z spira: state: pass\n'
    printf 'FAILED sp-supersede-key  branch and worktree\n'
}
read -r s h k f <<< "$(fixture_two_stuck | run_fixture)"
is "a second distinct branch IS seen" "2" "$f"

# ---------------------------------------------------------------------------------------
# THE SUMMARY LINE ALONE IS NOT A FIEND. sentinel.sh's per-pass line carries no branch id;
# it is a report ABOUT the output above it. Counting it was half of the doubling.
# ---------------------------------------------------------------------------------------
read -r s h k f <<< "$(printf '2026-09-07T01:00:00Z spira: state: pass\n2026-09-07T01:00:01Z spira: sending reported a branch it could not delete\n' | run_fixture)"
is "the per-pass summary alone counts nothing" "0" "$f"

# ---------------------------------------------------------------------------------------
# SENT HAS THE SAME DEFECT AND THE SAME FIX. A branch re-sent across passes is one sending.
# ---------------------------------------------------------------------------------------
fixture_resent() {
    for i in 1 2 3; do
        printf '2026-09-07T0%d:00:00Z spira: state: pass\n' "$i"
        printf 'SENT sp-abc  branch and worktree\n'
    done
    printf '2026-09-07T05:00:00Z spira: state: pass\n'
    printf 'SENT sp-xyz  branch and worktree\n'
}
read -r s h k f <<< "$(fixture_resent | run_fixture)"
is "a branch sent on three passes is ONE sending" "2" "$s"

# ---------------------------------------------------------------------------------------
# HELD/KEPT ALREADY COUNTED BRANCHES. Asserted here so a future edit to this function cannot
# regress them silently — they are the precedent this fix follows.
# ---------------------------------------------------------------------------------------
fixture_held() {
    for i in 1 2 3 4; do
        printf '2026-09-07T0%d:00:00Z spira: state: pass\n' "$i"
        printf 'HELD sp-live  a live aeon holds it\n'
        printf 'KEEP sp-unlanded  2 commit(s) not in base\n'
    done
}
read -r s h k f <<< "$(fixture_held | run_fixture)"
is "held counts branches, not passes" "1" "$h"
is "kept counts branches, not passes" "1" "$k"

# ---------------------------------------------------------------------------------------
# SP_SENT_FAILED_AGE_M — age of the most recent FAILED line.
# The alarm in health.sh dims once the age exceeds 60m, so the age must be correct for
# both the "old window" case (all failures resolved hours ago) and the "recent" case.
# now is 2026-09-08T12:00:00Z in run_fixture_age.
# ---------------------------------------------------------------------------------------

# OLD WINDOW: every FAILED line is from the 16:31Z pass of the PREVIOUS DAY (2026-09-07),
# which is 2026-09-07T16:31:00Z — that is 19h29m before 2026-09-08T12:00:00Z = 1169m.
fixture_old_failures() {
    for i in 1 2 3; do
        printf '2026-09-07T16:3%d:00Z spira: state: pass\n' "$i"
        printf 'FAILED sp-kq8l  branch and worktree\n'
        printf '2026-09-07T16:3%d:01Z spira: sending reported a branch it could not delete\n' "$i"
    done
}
read -r f age <<< "$(fixture_old_failures | run_fixture_age)"
is "old window counts the branch" "1" "$f"
is "old window age is minutes since last pass (1167)" "1167" "$age"

# RECENT FAILURE: a FAILED line 10 minutes before now.
fixture_recent_failure() {
    printf '2026-09-08T11:50:00Z spira: state: pass\n'
    printf 'FAILED sp-xyz  branch and worktree\n'
}
read -r f age <<< "$(fixture_recent_failure | run_fixture_age)"
is "recent failure counts" "1" "$f"
is "recent failure age is 10m" "10" "$age"

# MULTIPLE PASSES: age tracks the MOST RECENT FAILED line, not the first.
fixture_multi_pass_failures() {
    printf '2026-09-08T10:00:00Z spira: state: pass\n'
    printf 'FAILED sp-abc  branch and worktree\n'
    printf '2026-09-08T11:45:00Z spira: state: pass\n'
    printf 'FAILED sp-abc  branch and worktree\n'
}
read -r f age <<< "$(fixture_multi_pass_failures | run_fixture_age)"
is "multi-pass: still one branch" "1" "$f"
is "multi-pass: age is from the LAST pass (15m ago)" "15" "$age"

# NO FAILURES: age is ? when no FAILED lines appear.
fixture_no_failures() {
    printf '2026-09-08T11:00:00Z spira: state: pass\n'
    printf 'SENT sp-abc  branch\n'
}
read -r f age <<< "$(fixture_no_failures | run_fixture_age)"
is "no failures: count is 0" "0" "$f"
is "no failures: age is ?" "?" "$age"
tl_summary
