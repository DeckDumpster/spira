#!/usr/bin/env bash
#
# test-suites-timer.sh — spira-suites.timer exists, fires periodically, and invokes
# suites.sh run via spira-suites.service.
#
#   ./test-suites-timer.sh
#
# WHAT THIS GUARDS. The defect (sp-mfa4) was that nothing invoked suites.sh run: the timed
# suite set existed in code but had no timer, so every "run by the timed runner" claim was
# fiction. The failure mode is perfectly silent: suites.sh status reports "never ran" as "?"
# for every result, which reads as healthy until someone notices that ? never changes.
#
# THREE PROPERTIES, each with its positive control:
#
#   1. THE SERVICE INVOKES suites.sh run — the exact subcommand, not just the script. A
#      service that calls suites.sh with no argument calls `list`, not `run`, and produces
#      beautiful output while running nothing.
#
#   2. THE TIMER FIRES PERIODICALLY — OnUnitActiveSec is present. A timer with only
#      OnBootSec fires once after boot and then never again, which is indistinguishable from
#      a missing timer after the first pass.
#
#   3. THE TIMER IS IN THE INSTALL ENABLE LIST — install.sh enables it. A unit that is
#      installed but not enabled is, again, a timer that never fires. The list is read from
#      install.sh's source rather than from the running system so this check survives a clean
#      clone or a CI run with no systemd.
#
# POSITIVE CONTROL IS FIRST IN EVERY CASE (law-absence-needs-a-positive-control). A parser
# that returns "" on any input passes an "is it in the list" assertion just as well, because
# the assertion is on absence (the thing would not be absent). So each property proves the
# extractor can find a KNOWN entry before trusting a verdict of "present".
#
# THE CHECK IS AGAINST THE TEMPLATE FILES, not the running system. Templates are the source;
# the rendered, installed units derive from them. A test that requires an active systemd
# session passes nowhere the harness runs unattended.
#
# defect: sp-mfa4
# covers: systemd/spira-suites.timer systemd/spira-suites.service systemd/install.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
UNIT_DIR="$HERE/../systemd"
INSTALL_SH="$UNIT_DIR/install.sh"
UNITS_SH="$UNIT_DIR/units.sh"

pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "${2:-}"; }
is()  { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want() { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

echo "test-suites-timer.sh"

# ============================================================================
echo
echo "spira-suites.service invokes suites.sh run (not list, not bare):"
# ============================================================================
SVC="$UNIT_DIR/spira-suites.service"
[ -r "$SVC" ] || { bad "spira-suites.service is readable" "not found at $SVC"; }

# POSITIVE CONTROL: the ExecStart line is there and parseable before we trust
# a content assertion. A service with no ExecStart is not a service.
execstart="$(grep '^ExecStart=' "$SVC" 2>/dev/null | head -1)"
if [ -z "$execstart" ]; then
    bad "spira-suites.service has an ExecStart line" "none found"
else
    ok "spira-suites.service has an ExecStart line"

    # The subcommand must be `run`, not absent or another word. `suites.sh list`
    # produces output while running nothing; `suites.sh` alone does the same.
    want "ExecStart invokes suites.sh run" "suites.sh run" "$execstart"

    # Guard: `suites.sh run` must not be followed by additional subcommands that
    # would change the meaning — `suites.sh run list` is not valid.
    # (The template placeholder @SPIRA_PROD@ is expected here; that is fine.)
    case "$execstart" in
        *"suites.sh run "*) bad "ExecStart ends at run (no extra arguments)" "$execstart" ;;
        *"suites.sh run")   ok  "ExecStart ends at run (no extra arguments)" ;;
    esac
fi

# ============================================================================
echo
echo "spira-suites.timer fires periodically via OnUnitActiveSec:"
# ============================================================================
TMR="$UNIT_DIR/spira-suites.timer"
[ -r "$TMR" ] || { bad "spira-suites.timer is readable" "not found at $TMR"; }

# POSITIVE CONTROL: the file is parseable — OnBootSec is present (a file without
# this could not start the service at all, so its absence is a broken timer rather
# than a "never fires" timer).
onbootsec="$(grep '^OnBootSec=' "$TMR" 2>/dev/null | head -1)"
if [ -z "$onbootsec" ]; then
    bad "spira-suites.timer has OnBootSec (positive control)" "none found"
else
    ok "spira-suites.timer has OnBootSec ($onbootsec)"

    # THE PROPERTY THAT MATTERS. OnUnitActiveSec is the repeat interval; a timer
    # without it fires once per boot and never again — the silent shape that is
    # indistinguishable from no timer at all after that first pass.
    onactive="$(grep '^OnUnitActiveSec=' "$TMR" 2>/dev/null | head -1)"
    if [ -z "$onactive" ]; then
        bad "spira-suites.timer has OnUnitActiveSec (fires periodically, not once)" \
            "directive absent — timer fires once per boot only"
    else
        ok "spira-suites.timer has OnUnitActiveSec ($onactive)"
    fi
fi

# THE TIMER NAMES THE SERVICE. A timer whose Unit= points elsewhere activates
# something other than the suites runner.
unit_line="$(grep '^Unit=' "$TMR" 2>/dev/null | head -1 | cut -d= -f2- | tr -d '[:space:]')"
if [ -z "$unit_line" ]; then
    bad "spira-suites.timer has a Unit= directive" "absent"
else
    ok "spira-suites.timer has Unit=$unit_line"
    want "Unit= names the suites service" "spira-suites" "$unit_line"
fi

# ============================================================================
echo
echo "spira-suites.timer is in units.sh's enable list:"
# ============================================================================
[ -r "$UNITS_SH" ] || { bad "units.sh is readable" "not found at $UNITS_SH"; }

# Parse the _ENABLE_TMPL array from units.sh source. The pattern is:
#   _ENABLE_TMPL=(... spira-sentinel.timer ... spira-suites.timer ...)
# across multiple continuation lines. We extract the block and check for each name.
# NOTE: units.sh unsets _ENABLE_TMPL after building ENABLE, so this reads the source
# file, not a live shell variable.
enable_block="$(awk '/_ENABLE_TMPL=\(/{found=1} found{print} found && /\)/{found=0}' \
    "$UNITS_SH" 2>/dev/null)"

# POSITIVE CONTROL: a timer known to be in the list (spira-sentinel.timer) must
# appear in the parsed block before any absence verdict is trusted. A parser that
# returns an empty block would make every "present" check fail loudly, which is
# fine; the danger is a parser that returns something that looks plausible but
# matches nothing — so sentinel is the canary.
if [ -z "$enable_block" ]; then
    bad "_ENABLE_TMPL block is parseable (positive control)" "awk found nothing"
else
    ok "_ENABLE_TMPL block is parseable (${#enable_block} bytes)"

    case "$enable_block" in
        *spira-sentinel.timer*)
            ok "positive control: spira-sentinel.timer is in the enable list" ;;
        *)
            bad "positive control: spira-sentinel.timer is in the enable list" \
                "not found — the parser may be broken" ;;
    esac

fi

# spira-suites.timer is conditionally added (SPIRA_SELF_TEST path), not in _ENABLE_TMPL.
# Check comment-stripped source for the conditional ENABLE+= and UNITS+= lines.
units_code="$(sed 's/#.*//; /^[[:space:]]*$/d' "$UNITS_SH" 2>/dev/null)"
if [ -z "$units_code" ]; then
    bad "units.sh code is readable (positive control)" "sed found nothing"
else
    ok "units.sh code is readable (${#units_code} bytes)"

    # POSITIVE CONTROL: spira-promote.timer is also conditionally added to ENABLE.
    promote_in_enable="$(printf '%s\n' "$units_code" | grep 'spira-promote.timer' | grep 'ENABLE')"
    case "$promote_in_enable" in
        ?*) ok "positive control: spira-promote.timer has a conditional ENABLE+= line" ;;
        *)  bad "positive control: spira-promote.timer conditional ENABLE+= line" \
                "not found — the grep may be broken" ;;
    esac

    suites_in_enable="$(printf '%s\n' "$units_code" | grep 'spira-suites.timer' | grep 'ENABLE')"
    case "$suites_in_enable" in
        ?*) ok "spira-suites.timer is added to ENABLE in SPIRA_SELF_TEST path" ;;
        *)  bad "spira-suites.timer added to ENABLE in SPIRA_SELF_TEST path" \
                "no ENABLE+= line — timer will not be enabled in development mode" ;;
    esac
fi

# A unit in the enable path must also be in UNITS. Enabled means nothing
# if the file is never written to ~/.config/systemd/user in the first place.
units_block="$(awk '/^UNITS=\(/{found=1} found{print} found && /\)/{found=0}' \
    "$UNITS_SH" 2>/dev/null)"
if [ -z "$units_block" ]; then
    bad "UNITS block is parseable (positive control)" "awk found nothing"
else
    # Positive control again: the sentinel is in UNITS.
    case "$units_block" in
        *spira-sentinel.timer*)
            ok "positive control: spira-sentinel.timer is in UNITS" ;;
        *)
            bad "positive control: spira-sentinel.timer is in UNITS" \
                "not found — the parser may be broken" ;;
    esac

    # spira-suites.timer and .service are added via UNITS+=, not the static UNITS=().
    # POSITIVE CONTROL: spira-promote.timer is also conditionally added to UNITS.
    promote_in_units="$(printf '%s\n' "$units_code" | grep 'spira-promote.timer' | grep 'UNITS')"
    case "$promote_in_units" in
        ?*) ok "positive control: spira-promote.timer has a conditional UNITS+= line" ;;
        *)  bad "positive control: spira-promote.timer conditional UNITS+= line" \
                "not found — the grep may be broken" ;;
    esac

    suites_in_units="$(printf '%s\n' "$units_code" | grep 'spira-suites.timer' | grep 'UNITS')"
    case "$suites_in_units" in
        ?*) ok "spira-suites.timer is added to UNITS in SPIRA_SELF_TEST path" ;;
        *)  bad "spira-suites.timer added to UNITS in SPIRA_SELF_TEST path" \
                "no UNITS+= line — install will not write the timer file" ;;
    esac

    suites_svc_in_units="$(printf '%s\n' "$units_code" | grep 'spira-suites.service' | grep 'UNITS')"
    case "$suites_svc_in_units" in
        ?*) ok "spira-suites.service is added to UNITS in SPIRA_SELF_TEST path" ;;
        *)  bad "spira-suites.service added to UNITS in SPIRA_SELF_TEST path" \
                "no UNITS+= line — install will not write the service file" ;;
    esac
fi

# ============================================================================
echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
