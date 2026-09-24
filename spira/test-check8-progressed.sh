#!/usr/bin/env bash
#
# test-check8-progressed.sh — CHECK 8 fires when acted > 0 but progressed == 0;
#   does NOT fire when progressed > 0.
#
#   ./test-check8-progressed.sh
#
# WHY THIS EXISTS. Before sp-acted-conflation, sentinel.sh kept a single `acted` counter
# and gated CHECK 8 (judgement/inference) on `acted == 0`. Any false positive from any
# earlier check — the reclaim probe matching its own idle message, the Sending failing to
# delete a locked branch — incremented `acted` and silenced the one check that notices
# the system is stuck.
#
# The fix splits the counter: `acted` records any write; `progressed` records only a real
# DAG movement (a bead changed status, a branch landed). CHECK 8 gates on `progressed == 0`,
# never on `acted == 0`.
#
# TWO CASES, BOTH SIDES EXERCISED:
#   1. ACTED-WITHOUT-PROGRESS — the Sending emits a SENT line (→ act() called, progressed
#      stays 0). CHECK 8 must fire. Without this half, a regression that gates CHECK 8 on
#      acted == 0 again is indistinguishable from the correct code.
#
#   2. POSITIVE CONTROL (progressed > 0) — a line in the landing mailbox causes land_drain
#      to call progress(), but the harness is otherwise starved. CHECK 8 must NOT fire.
#      Without this half, a check-8 that always fires would pass case 1 for the wrong reason.
#
# FIXTURE. A goal epic with one open child bead carrying the ask label. The ask label
# excludes it from plan_ready (ready_count strips it), so plan_ready=0, plan_inprog=0 and
# n_open=1 on every pass — the starved state that should reach CHECK 8 each time. No
# blocking dep is needed; the label exclusion is cheaper and just as reliable.
#
# A REAL bd ON A FIXTURE DATABASE (law-prefer-the-real-dependency). plan_ready and
# plan_inprog are read from bd; a stub would drift silently and prove nothing about the
# path that mattered.
#
# defect: sp-acted-conflation
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
testdb_require test-check8-progressed
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up check8progressed || { echo "test-check8-progressed: could not build fixture database"; exit 1; }

# Seed: goal epic with one open child carrying the ask label.
# The ask label excludes sp-work from ready_count (plan_ready=0).
# sp-work is open (plan_inprog=0) and a child of sp-goal (n_open=1).
# This is the starved state that must reach CHECK 8 on every pass.
testdb_reset
testdb_seed <<'JSONL'
{"id":"sp-goal","title":"test goal epic","status":"open","issue_type":"epic","labels":["plan","spira"]}
{"id":"sp-work","title":"stalled work","status":"open","issue_type":"task","labels":["plan","spira","needs-operator"],"dependencies":[{"issue_id":"sp-work","depends_on_id":"sp-goal","type":"parent-child"}]}
JSONL

# STUBS directory: scripts sentinel.sh calls via $SPIRA_HOME.
# Each exits 0 and produces no output so no CHECK fires an action from them,
# unless the test case overrides sending.sh below.
STUBS="$TMP/stubs"
mkdir -p "$STUBS/chamber"   # empty chamber → no fayths → no CHECK 7 act()s

for _name in pilgrimage.sh strand.sh; do
    printf '#!/bin/sh\n# hermetic-ok: stub for test-check8-progressed\n' \
        > "$STUBS/$_name"
    chmod +x "$STUBS/$_name"
done
# mock-systemctl always says the service is inactive so the landing dispatch path runs.
printf '#!/bin/sh\n# hermetic-ok: stub for test-check8-progressed\necho inactive\n' \
    > "$STUBS/mock-systemctl"
chmod +x "$STUBS/mock-systemctl"
for _name in mock-launch mock-summon mock-notify; do
    printf '#!/bin/sh\n# hermetic-ok: stub for test-check8-progressed\nexit 0\n' \
        > "$STUBS/$_name"
    chmod +x "$STUBS/$_name"
done
# Empty repo-map so repo_root returns failure — CHECK 5 / CHECK 6 never reach repo_root.
touch "$STUBS/repo-map"

# reflect.sh stub records that it was called.
# sentinel.sh calls: "$SPIRA_HOME/reflect.sh" "$open_children" >> "$SPIRA_RUN/reflect.log"
# The stub touches $SPIRA_RUN/reflect.called so the test can witness the call.
cat > "$STUBS/reflect.sh" <<'SH'
#!/bin/sh
# hermetic-ok: stub for test-check8-progressed
touch "$SPIRA_RUN/reflect.called"
SH
chmod +x "$STUBS/reflect.sh"

# sending.sh default: no output (safe no-op).
printf '#!/bin/sh\n# hermetic-ok: stub for test-check8-progressed\n' \
    > "$STUBS/sending.sh"
chmod +x "$STUBS/sending.sh"

# run_sentinel [mailbox_line] — run sentinel.sh with a minimal environment.
# If mailbox_line is non-empty, pre-seeds $SPIRA_RUN/landing.progress so land_drain
# calls progress() on the first drain, setting progressed > 0 before CHECK 8 is reached.
#
# Output is written to $TMP/last-out; the run directory is stored in $_last_run.
# Called directly (never in $(...)), so both _run_cnt and _last_run are visible to the
# caller. A subshell call would reset _run_cnt to 0 on every case, colliding both cases
# on run-1 and leaving case 2 seeing case 1's reflect.called file.
_run_cnt=0; _last_run=""
run_sentinel() {
    local mailbox_line="${1:-}"
    _run_cnt=$((_run_cnt + 1))
    local run="$TMP/run-$_run_cnt"; mkdir -p "$run"
    _last_run="$run"

    if [ -n "$mailbox_line" ]; then
        printf '%s\n' "$mailbox_line" > "$run/landing.progress"
    fi

    # Explicit minimal environment (law-gates-run-in-a-clean-environment).
    # SPIRA_INFERENCE_EVERY=0 ensures the cooldown never blocks CHECK 8 even if the
    # fixture database timestamps make (now - last) look small.
    # SPIRA_ASK_LABEL is pinned to match the fixture bead's label so the real spira.conf
    # (which may set it to something else) cannot silently make plan_ready > 0. The fixture
    # bead carries "needs-operator"; a host whose SPIRA_ASK_LABEL differs would not
    # exclude it, leaving plan_ready=1 and causing an early exit before CHECK 8 is reached.
    env -i \
        PATH="$PATH" HOME="$HOME" \
        SPIRA_HOME="$STUBS" \
        SPIRA_RUN="$run" \
        SPIRA_DB="$SPIRA_DB" \
        SPIRA_BD="$SPIRA_BD" \
        SPIRA_PATH="$SPIRA_PATH" \
        SPIRA_GOAL="sp-goal" \
        SPIRA_FAYTHS="" \
        SPIRA_LAND_STALE=999999 \
        SPIRA_INFERENCE_EVERY=0 \
        SPIRA_ASK_LABEL=needs-operator \
        SPIRA_SYSTEMCTL="$STUBS/mock-systemctl" \
        SPIRA_LAUNCH="$STUBS/mock-launch" \
        SPIRA_SUMMON="$STUBS/mock-summon" \
        SPIRA_NOTIFY="$STUBS/mock-notify" \
        bash "$HERE/sentinel.sh" > "$TMP/last-out" 2>&1
}

echo "test-check8-progressed.sh"

# ======================================================================================
echo
echo "case 1 — acted > 0, progressed == 0: CHECK 8 fires despite the Sending acting:"
# ======================================================================================
# The Sending emits a SENT line. sentinel.sh reads it and calls act(), so acted > 0.
# No landing mailbox line means progressed stays 0.
#
# Under the old code (CHECK 8 gated on acted == 0), a SENT would silence judgement for
# the entire pass. Under the fix, CHECK 8 gates on progressed == 0 and must fire.
#
# THE SENT LINE FORMAT is: "SENT <id> <repo> <branch>", matching what sending.sh emits
# and what sentinel.sh parses with `read -r _ rid rrepo rbr _`.
printf '#!/bin/sh\nprintf "SENT sp-work spira spira/sp-work\\n"\n' \
    > "$STUBS/sending.sh"
chmod +x "$STUBS/sending.sh"

run_sentinel ""
out="$(cat "$TMP/last-out")"
want "log says STARVED (CHECK 8 fired)"          "STARVED" "$out"
is   "reflect.sh was called"                     "yes" "$([ -f "$_last_run/reflect.called" ] && echo yes || echo no)"

# ======================================================================================
echo
echo "case 2 — progressed > 0: CHECK 8 does NOT fire:"
# ======================================================================================
# A line in the landing mailbox causes land_drain to call progress(), so progressed > 0
# before CHECK 8 is evaluated. The harness is still nominally starved (plan_ready=0,
# plan_inprog=0, n_open=1), but the DAG moved — CHECK 8 must stay silent.
#
# Without this half, a check-8 that always fired would pass case 1 for the wrong reason.
printf '#!/bin/sh\n# hermetic-ok: stub for test-check8-progressed\n' \
    > "$STUBS/sending.sh"
chmod +x "$STUBS/sending.sh"

run_sentinel "landed spira/sp-other"
out="$(cat "$TMP/last-out")"
lack "log does NOT say STARVED (CHECK 8 suppressed)" "STARVED" "$out"
is   "reflect.sh was NOT called"                     "no" "$([ -f "$_last_run/reflect.called" ] && echo yes || echo no)"

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
