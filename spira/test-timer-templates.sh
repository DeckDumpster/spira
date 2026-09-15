#!/usr/bin/env bash
#
# test-timer-templates.sh — every .timer template in systemd/ is in units.sh's
# UNITS and _ENABLE_TMPL lists, fires periodically, and (when systemd is
# available) has been observed to have fired.
#
#   ./test-timer-templates.sh
#
# WHAT THIS GUARDS
# ----------------
# Defect sp-7gklu: timer template files existed in systemd/ but were absent from
# units.sh's UNITS array, so install.sh never wrote them to disk on a fresh install.
# skew.sh reported MISSING but nothing automatically verified that the full chain
# template → UNITS → _ENABLE_TMPL → installed → enabled was intact for every timer.
#
# THREE STATIC PROPERTIES (run in CI with no systemd):
#
#   1. EVERY TEMPLATE IS IN UNITS. A template absent from UNITS is never written
#      to ~/.config/systemd/user — it exists only on the machine where someone
#      added it by hand. Every other operator's install silently lacks it.
#
#   2. EVERY TEMPLATE IS IN _ENABLE_TMPL. A unit installed but not enabled starts
#      only when triggered manually. After any reboot it is silent, and its
#      silence is indistinguishable from the silence of a unit that was never
#      installed.
#
#   3. EVERY TEMPLATE HAS A PERIODIC FIRING MECHANISM. A timer with only
#      OnBootSec fires once at boot and then never again — the same silence, and
#      the same invisible failure.
#
# ONE LIVE PROPERTY (when systemd --user is reachable):
#
#   4. EVERY INSTALLED TIMER HAS A RECORDED LastTriggerUSec. An installed, enabled
#      timer with no LastTriggerUSec has never fired since boot — the exact failure
#      law-timers-active-is-not-running guards against. ActiveState is explicitly
#      NOT checked: a timer reads active while every run of its service fails.
#
# POSITIVE CONTROLS ARE FIRST (law-absence-needs-a-positive-control). The UNITS
# and _ENABLE_TMPL parsers are confirmed against spira-sentinel.timer (a known
# entry) before any absence verdict is trusted. A parser that returns empty or
# wrong content would make every timer look absent; the positive control catches
# that before a silent all-clear is emitted.
#
# THE STATIC CHECKS ARE THE PRIMARY ASSERTIONS. They work in any environment.
# The live section runs only when `systemctl --user` succeeds and is skipped
# gracefully when it does not.
#
# covers: systemd/*.timer systemd/units.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
UNIT_DIR="$HERE/../systemd"
UNITS_SH="$UNIT_DIR/units.sh"

pass=0; fail=0; skip=0
ok()   { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()  { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "${2:-}"; }
note() { skip=$((skip+1)); printf '  skip  %s\n' "$1"; }

echo "test-timer-templates.sh"

# ============================================================================
echo
echo "Parse units.sh — extract UNITS and _ENABLE_TMPL:"
# ============================================================================

[ -r "$UNITS_SH" ] || { printf '  FAIL  units.sh not readable at %s\n' "$UNITS_SH"; exit 1; }

units_block="$(awk '/^UNITS=\(/{found=1} found{print} found && /\)/{found=0}' "$UNITS_SH")"
enable_block="$(awk '/_ENABLE_TMPL=\(/{found=1} found{print} found && /\)/{found=0}' "$UNITS_SH")"

# CONDITIONAL UNITS ARE STILL INSTALLED UNITS. Some units belong on a box only in a
# particular shape — promote.sh has nothing to do where development and production are
# one checkout — so units.sh appends them with UNITS+=/ENABLE+= inside an if, and records
# the declined case in OPTIONAL+=. The static blocks above cannot see those lines, and a
# parser that stops at the literal array would report a correctly conditional timer as
# missing, which is the same cry-wolf this suite exists to prevent.
#
# So the membership test reads the appends too, and a timer that is ONLY conditional must
# ALSO appear in an OPTIONAL+= line. That keeps the original invariant intact — a unit is
# never silently absent — while letting a box decline one on purpose and say so.
units_appends="$(grep -E '^[[:space:]]*UNITS\+=\(' "$UNITS_SH")"
enable_appends="$(grep -E '^[[:space:]]*ENABLE\+=\(' "$UNITS_SH")"
optional_block="$(grep -E '^[[:space:]]*OPTIONAL\+=\(' "$UNITS_SH")"
units_all="$units_block
$units_appends"
enable_all="$enable_block
$enable_appends"

if [ -z "$units_block" ]; then
    bad "UNITS block parseable" "awk found nothing — remaining checks are invalid"
    printf '\n%d passed, %d failed, %d skipped\n' "$pass" "$fail" "$skip"
    exit 1
fi
ok "UNITS block parseable (${#units_block} bytes)"

if [ -z "$enable_block" ]; then
    bad "_ENABLE_TMPL block parseable" "awk found nothing — remaining checks are invalid"
    printf '\n%d passed, %d failed, %d skipped\n' "$pass" "$fail" "$skip"
    exit 1
fi
ok "_ENABLE_TMPL block parseable (${#enable_block} bytes)"

# POSITIVE CONTROLS. spira-sentinel.timer is the canary: it must appear in both
# blocks before any absence verdict is trusted. If the parsers are broken or the
# blocks are misidentified, the canary fails loudly rather than silently passing.
case "$units_block" in
    *spira-sentinel.timer*)
        ok "positive control: spira-sentinel.timer is in UNITS" ;;
    *)
        bad "positive control: spira-sentinel.timer is in UNITS — parser may be broken" ""
        printf '\n%d passed, %d failed, %d skipped\n' "$pass" "$fail" "$skip"
        exit 1 ;;
esac

case "$enable_block" in
    *spira-sentinel.timer*)
        ok "positive control: spira-sentinel.timer is in _ENABLE_TMPL" ;;
    *)
        bad "positive control: spira-sentinel.timer is in _ENABLE_TMPL — parser may be broken" ""
        printf '\n%d passed, %d failed, %d skipped\n' "$pass" "$fail" "$skip"
        exit 1 ;;
esac

# ============================================================================
echo
echo "Every .timer template in systemd/ is in UNITS (will be installed):"
# ============================================================================

timer_count=0
for tmr in "$UNIT_DIR"/*.timer; do
    [ -e "$tmr" ] || continue
    name="$(basename "$tmr")"
    timer_count=$((timer_count+1))
    case "$units_all" in
        *"$name"*)
            case "$units_block" in
                *"$name"*) ok "UNITS: $name" ;;
                *)
                    # Conditional: permitted, but the declined case must be recorded.
                    case "$optional_block" in
                        *"$name"*) ok "UNITS: $name (conditional, declined case in OPTIONAL)" ;;
                        *) bad "UNITS: $name" "added conditionally but never recorded in OPTIONAL — a box that declines it cannot tell 'not installed here' from 'nobody listed it'" ;;
                    esac ;;
            esac ;;
        *)
            bad "UNITS: $name" "absent from UNITS — install.sh will not write it to disk on a fresh install" ;;
    esac
done

if [ "$timer_count" -eq 0 ]; then
    bad "at least one .timer file in $UNIT_DIR" "glob matched nothing — check the path"
else
    ok "$timer_count .timer templates found and checked against UNITS"
fi

# ============================================================================
echo
echo "Every .timer template in systemd/ is in _ENABLE_TMPL (will be enabled):"
# ============================================================================

for tmr in "$UNIT_DIR"/*.timer; do
    [ -e "$tmr" ] || continue
    name="$(basename "$tmr")"
    case "$enable_all" in
        *"$name"*)
            ok "_ENABLE_TMPL: $name" ;;
        *)
            bad "_ENABLE_TMPL: $name" \
                "absent — install.sh will install but not enable it; the timer will not fire" ;;
    esac
done

# ============================================================================
echo
echo "Every .timer template fires periodically (OnUnitActiveSec or OnCalendar):"
# ============================================================================

for tmr in "$UNIT_DIR"/*.timer; do
    [ -e "$tmr" ] || continue
    name="$(basename "$tmr")"
    periodic="$(grep -E '^OnUnitActiveSec=|^OnCalendar=' "$tmr" 2>/dev/null | head -1)"
    if [ -z "$periodic" ]; then
        bad "periodic: $name" \
            "OnUnitActiveSec and OnCalendar both absent — fires once at boot and then never again"
    else
        ok "periodic: $name (${periodic%%=*})"
    fi
done

# ============================================================================
echo
echo "Live: every installed timer has a recorded LastTriggerUSec:"
# ============================================================================

# SC is the systemctl command. When the caller sets SPIRA_SYSTEMCTL (e.g. to a
# real systemctl for manual verification against the live box), that is used.
# Otherwise a hermetic stub is created in a scratch directory: it simulates a
# fully-installed Spira so the assertion logic is exercised without depending
# on box state, which would make the gate's verdict a fact about the machine
# rather than the branch (law-gates-run-in-a-clean-environment).
_sc_tmp="$(mktemp -d)"
trap 'rm -rf "$_sc_tmp"' EXIT
if [ -z "${SPIRA_SYSTEMCTL:-}" ]; then
    cat > "$_sc_tmp/systemctl" << 'STUB'
#!/usr/bin/env bash
# Hermetic stub: simulates systemd --user for test-timer-templates.sh.
shift  # remove --user
case "$1" in
    status)
        exit 0 ;;
    list-unit-files)
        # $2 is the unit name; $3 is --no-legend (ignored)
        printf '%s enabled\n' "${2:-unknown.timer}"
        exit 0 ;;
    show)
        printf 'LastTriggerUSec=Thu 2026-09-11 10:00:00 PDT\n'
        exit 0 ;;
    *)
        exit 1 ;;
esac
STUB
    chmod +x "$_sc_tmp/systemctl"
fi
SC="${SPIRA_SYSTEMCTL:-$_sc_tmp/systemctl}"

# POSITIVE CONTROL for the live section. Confirm the stub/systemctl responds to
# "status" before trusting absence verdicts below. A stub that always fails here
# would make every timer appear uninstalled.
if ! "$SC" --user status >/dev/null 2>&1; then
    note "systemctl --user unreachable — skipping live-system assertions"
    printf '\n%d passed, %d failed, %d skipped\n' "$pass" "$fail" "$skip"
    [ "$fail" -eq 0 ]
    exit
fi

# Mirrors inst_name() from units.sh: spira-*.timer → spira-*-<instance>.timer;
# shared units (beads-push, cockpit-ensure, concierge) keep their template name.
# SPIRA_INSTANCE from the environment takes precedence; the default is prod.
: "${SPIRA_INSTANCE:=prod}"
_live_inst_name() {
    local u="$1"
    case "$u" in
        spira-*.timer) printf '%s-%s.timer' "${u%.timer}" "$SPIRA_INSTANCE" ;;
        *)             printf '%s' "$u" ;;
    esac
}

sentinel_inst="$(_live_inst_name spira-sentinel.timer)"
sentinel_list="$("$SC" --user list-unit-files "$sentinel_inst" --no-legend 2>/dev/null)"
if [ -z "$sentinel_list" ]; then
    note "live positive control: $sentinel_inst not in list-unit-files — skipping live checks"
    printf '\n%d passed, %d failed, %d skipped\n' "$pass" "$fail" "$skip"
    [ "$fail" -eq 0 ]
    exit
fi
ok "live positive control: $sentinel_inst is in list-unit-files"

for tmr in "$UNIT_DIR"/*.timer; do
    [ -e "$tmr" ] || continue
    name="$(basename "$tmr")"
    installed="$(_live_inst_name "$name")"

    # INSTALLED — appears in list-unit-files. Absent means install.sh was never
    # run for this template, or the template was added after the last install.
    list_out="$("$SC" --user list-unit-files "$installed" --no-legend 2>/dev/null)"
    if [ -z "$list_out" ]; then
        bad "live installed: $installed" "not in systemctl --user list-unit-files"
        continue
    fi
    ok "live installed: $installed"

    # ENABLED — install.sh's enable step ran for this unit. A unit in list-unit-
    # files with state 'disabled' was written to disk but never enabled; it will
    # not start at login and will never fire.
    state="$(printf '%s' "$list_out" | awk '{print $2}')"
    if [ "$state" = "enabled" ]; then
        ok "live enabled: $installed"
    else
        bad "live enabled: $installed" "state is '$state', not 'enabled'"
    fi

    # HAS FIRED — LastTriggerUSec is non-empty. ActiveState is deliberately not
    # checked here: a timer reads active even while every run of its service
    # fails (law-timers-active-is-not-running). LastTriggerUSec records the last
    # actual activation; an empty value means the timer has never fired since boot.
    last_trigger="$("$SC" --user show "$installed" \
                        --property=LastTriggerUSec 2>/dev/null \
                    | sed 's/^LastTriggerUSec=//')"
    if [ -n "$last_trigger" ] && [ "$last_trigger" != "n/a" ]; then
        ok "live has fired: $installed (LastTriggerUSec=$last_trigger)"
    else
        bad "live has fired: $installed" \
            "LastTriggerUSec is empty — timer has not fired since boot"
    fi
done

echo
printf '%d passed, %d failed, %d skipped\n' "$pass" "$fail" "$skip"
[ "$fail" -eq 0 ]
