#!/usr/bin/env bash
#
# test-verdict-timer.sh — spira-verdict.timer exists, fires periodically, and
# invokes queue.sh step via spira-verdict.sh.
#
#   ./test-verdict-timer.sh
#
# WHAT THIS GUARDS. The batch cycle equals the landing-pass duration when
# verdict.sh only runs inside the landing pass (~58 min). A dedicated 2-minute
# timer lets verdict.sh settle open batches as soon as CI completes (~18 min),
# reducing the cycle to CI wall + poll (~20 min). Without this test, the timer
# could be dropped from units.sh or the service could call the wrong subcommand,
# and nothing would report the regression.
#
# TWO PROPERTIES, each with its positive control:
#
#   1. THE SERVICE INVOKES queue.sh step — via spira-verdict.sh, which iterates
#      queue-mode repos. A service that calls verdict.sh directly skips batch.sh,
#      leaving no new batch open after a landing (next batch waits for the landing
#      pass). A service that calls the wrong subcommand runs nothing.
#
#   2. THE SERVICE LIMITS COVER THE ATTRIBUTION BUDGET (sp-vhvyi) — no CPUQuota,
#      and a TimeoutStartSec long enough for a full red-batch replay.
#
# Timer install/enable/periodic-firing (UNITS, _ENABLE_TMPL, OnUnitActiveSec) is
# test-timer-templates.sh's generic loop over every systemd/*.timer file
# (duplicate cluster #16, docs/test-plan/landing-merge-queue.md section 4) —
# spira-verdict.timer is covered there and is not re-asserted here.
#
# POSITIVE CONTROL IS FIRST IN EVERY CASE (law-absence-needs-a-positive-control).
#
# defect: sp-tv7ue
# covers: systemd/spira-verdict.timer systemd/spira-verdict.service
#         spira/spira-verdict.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
UNIT_DIR="$HERE/../systemd"

pass=0; fail=0
ok()    { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()   { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "${2:-}"; }
want()  { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant(){ [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

echo "test-verdict-timer.sh"

# ============================================================================
echo
echo "spira-verdict.service invokes spira-verdict.sh (which calls queue.sh step):"
# ============================================================================
SVC="$UNIT_DIR/spira-verdict.service"
[ -r "$SVC" ] || { bad "spira-verdict.service is readable" "not found at $SVC"; }

execstart="$(grep '^ExecStart=' "$SVC" 2>/dev/null | head -1)"
if [ -z "$execstart" ]; then
    bad "spira-verdict.service has an ExecStart line" "none found"
else
    ok "spira-verdict.service has an ExecStart line"
    want "ExecStart invokes spira-verdict.sh" "spira-verdict.sh" "$execstart"
fi

# The driver script must call queue.sh step — that is the command that settles
# the open batch (verdict.sh) then opens the next (batch.sh). A script that
# calls only verdict.sh skips batch.sh and leaves no new batch after a landing.
DRIVER="$HERE/spira-verdict.sh"
[ -r "$DRIVER" ] || { bad "spira-verdict.sh is readable" "not found at $DRIVER"; }

driver_src="$(cat "$DRIVER" 2>/dev/null)"
if [ -z "$driver_src" ]; then
    bad "spira-verdict.sh is non-empty" "empty or unreadable"
else
    ok "spira-verdict.sh is non-empty"

    # POSITIVE CONTROL: the script sources lib.sh — a script that sources nothing
    # cannot call spira_repos or repo_land. Without this any absence verdict below
    # could be from a completely empty (but non-zero) file.
    want "positive control: spira-verdict.sh sources lib.sh" \
         "lib.sh" "$driver_src"

    want "spira-verdict.sh calls queue.sh step" \
         'queue.sh" step' "$driver_src"

    # The driver must iterate repos, not hard-code a single name. Absence of a
    # loop would mean only the home repo is ever checked.
    want "spira-verdict.sh iterates repos" \
         "spira_repos" "$driver_src"
fi

# ============================================================================
echo
echo "spira-verdict.timer targets the verdict service:"
# ============================================================================
TMR="$UNIT_DIR/spira-verdict.timer"
[ -r "$TMR" ] || { bad "spira-verdict.timer is readable" "not found at $TMR"; }

unit_line="$(grep '^Unit=' "$TMR" 2>/dev/null | head -1 | cut -d= -f2- | tr -d '[:space:]')"
if [ -z "$unit_line" ]; then
    bad "spira-verdict.timer has a Unit= directive" "absent"
else
    ok "spira-verdict.timer has Unit=$unit_line"
    want "Unit= names the verdict service" "spira-verdict" "$unit_line"
fi

# ============================================================================
echo
echo "spira-verdict.service limits cover the attribution budget (sp-vhvyi):"
# ============================================================================
# POSITIVE CONTROL (must come first): confirm the parser reads the real service
# file before checking for absence of CPUQuota. A completely empty file would pass
# the absence check vacuously; requiring Type= proves the file is non-empty.
if grep -q '^Type=' "$SVC" 2>/dev/null; then
    ok "positive control: Type= directive present (service file is readable)"
else
    bad "positive control: Type= directive present (service file is readable)" \
        "not found — service file may be empty or unparseable"
fi

timeout_line="$(grep '^TimeoutStartSec=' "$SVC" 2>/dev/null | head -1)"
if [ -z "$timeout_line" ]; then
    bad "spira-verdict.service has TimeoutStartSec" "directive absent"
else
    ok "spira-verdict.service has TimeoutStartSec ($timeout_line)"
    timeout_val="${timeout_line#TimeoutStartSec=}"
    # The old value (120) would fail this check — that is the pair the bead requires.
    if [ "${timeout_val}" -ge 3600 ] 2>/dev/null; then
        ok "TimeoutStartSec >= 3600s (covers red-batch replay per member)"
    else
        bad "TimeoutStartSec >= 3600s (covers red-batch replay per member)" \
            "${timeout_val}s < 3600s — systemd kills every replay at two minutes (sp-vhvyi)"
    fi
fi

cpu_quota="$(grep '^CPUQuota=' "$SVC" 2>/dev/null | head -1)"
if [ -z "$cpu_quota" ]; then
    ok "spira-verdict.service has no CPUQuota (replay runs at full CPU)"
else
    bad "spira-verdict.service has no CPUQuota (replay runs at full CPU)" \
        "found: $cpu_quota — CPUQuota throttles containers during red-batch replay (sp-vhvyi)"
fi

# ============================================================================
echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
