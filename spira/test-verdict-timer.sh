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
# THREE PROPERTIES, each with its positive control:
#
#   1. THE SERVICE INVOKES queue.sh step — via spira-verdict.sh, which iterates
#      queue-mode repos. A service that calls verdict.sh directly skips batch.sh,
#      leaving no new batch open after a landing (next batch waits for the landing
#      pass). A service that calls the wrong subcommand runs nothing.
#
#   2. THE TIMER FIRES PERIODICALLY — OnUnitActiveSec is present. A timer with
#      only OnBootSec fires once at boot and never again.
#
#   3. THE TIMER IS IN THE INSTALL ENABLE LIST — install.sh enables it. A unit
#      that is installed but not enabled is a timer that never fires.
#
# POSITIVE CONTROL IS FIRST IN EVERY CASE (law-absence-needs-a-positive-control).
#
# defect: sp-tv7ue
# covers: systemd/spira-verdict.timer systemd/spira-verdict.service
#         spira/spira-verdict.sh systemd/units.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
UNIT_DIR="$HERE/../systemd"
UNITS_SH="$UNIT_DIR/units.sh"

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
echo "spira-verdict.timer fires periodically via OnUnitActiveSec:"
# ============================================================================
TMR="$UNIT_DIR/spira-verdict.timer"
[ -r "$TMR" ] || { bad "spira-verdict.timer is readable" "not found at $TMR"; }

onbootsec="$(grep '^OnBootSec=' "$TMR" 2>/dev/null | head -1)"
if [ -z "$onbootsec" ]; then
    bad "spira-verdict.timer has OnBootSec (positive control)" "none found"
else
    ok "spira-verdict.timer has OnBootSec ($onbootsec)"

    onactive="$(grep '^OnUnitActiveSec=' "$TMR" 2>/dev/null | head -1)"
    if [ -z "$onactive" ]; then
        bad "spira-verdict.timer has OnUnitActiveSec (fires periodically)" \
            "directive absent — timer fires once per boot only"
    else
        ok "spira-verdict.timer has OnUnitActiveSec ($onactive)"
    fi
fi

unit_line="$(grep '^Unit=' "$TMR" 2>/dev/null | head -1 | cut -d= -f2- | tr -d '[:space:]')"
if [ -z "$unit_line" ]; then
    bad "spira-verdict.timer has a Unit= directive" "absent"
else
    ok "spira-verdict.timer has Unit=$unit_line"
    want "Unit= names the verdict service" "spira-verdict" "$unit_line"
fi

# ============================================================================
echo
echo "spira-verdict.timer is in units.sh's enable list:"
# ============================================================================
[ -r "$UNITS_SH" ] || { bad "units.sh is readable" "not found at $UNITS_SH"; }

enable_block="$(awk '/_ENABLE_TMPL=\(/{found=1} found{print} found && /\)/{found=0}' \
    "$UNITS_SH" 2>/dev/null)"

if [ -z "$enable_block" ]; then
    bad "_ENABLE_TMPL block is parseable (positive control)" "awk found nothing"
else
    ok "_ENABLE_TMPL block is parseable (${#enable_block} bytes)"

    # POSITIVE CONTROL: a known entry must appear before any absence verdict.
    case "$enable_block" in
        *spira-sentinel.timer*)
            ok "positive control: spira-sentinel.timer is in the enable list" ;;
        *)
            bad "positive control: spira-sentinel.timer is in the enable list" \
                "not found — the parser may be broken" ;;
    esac

    case "$enable_block" in
        *spira-verdict.timer*)
            ok "spira-verdict.timer is in the enable list" ;;
        *)
            bad "spira-verdict.timer is in the enable list" \
                "not found — timer will not be enabled on install" ;;
    esac
fi

units_block="$(awk '/^UNITS=\(/{found=1} found{print} found && /\)/{found=0}' \
    "$UNITS_SH" 2>/dev/null)"
if [ -z "$units_block" ]; then
    bad "UNITS block is parseable (positive control)" "awk found nothing"
else
    case "$units_block" in
        *spira-sentinel.timer*)
            ok "positive control: spira-sentinel.timer is in UNITS" ;;
        *)
            bad "positive control: spira-sentinel.timer is in UNITS" \
                "not found — the parser may be broken" ;;
    esac

    case "$units_block" in
        *spira-verdict.timer*)
            ok "spira-verdict.timer is in UNITS" ;;
        *)
            bad "spira-verdict.timer is in UNITS" \
                "not found — install will not write the timer file" ;;
    esac

    case "$units_block" in
        *spira-verdict.service*)
            ok "spira-verdict.service is in UNITS" ;;
        *)
            bad "spira-verdict.service is in UNITS" \
                "not found — install will not write the service file" ;;
    esac
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
