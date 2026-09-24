#!/usr/bin/env bash
#
# test-mail-tidy-units.sh — the mail-tidy service and timer render correctly and are
# registered with install.sh. Split out of test-mail-tidy.sh (coverage-map row 08:
# "unit greps -> T0 unit lint") so a source-text check never pays for a Maildir fixture.
#
# tier: T0
# covers: systemd/spira-mail-tidy.service systemd/spira-mail-tidy.timer systemd/units.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
UNIT_DIR="$HERE/../systemd"

echo
echo "=== Service file: ExecStart invokes mail.sh tidy ==="

SVC="$UNIT_DIR/spira-mail-tidy.service"
[ -r "$SVC" ] || bad "spira-mail-tidy.service readable" "not found at $SVC"

execstart="$(grep '^ExecStart=' "$SVC" 2>/dev/null | head -1)"
if [ -z "$execstart" ]; then
    bad "spira-mail-tidy.service has ExecStart" "absent"
else
    ok "spira-mail-tidy.service has ExecStart"
    want "ExecStart calls mail.sh"        "mail.sh"  "$execstart"
    want "ExecStart calls tidy subcommand" "tidy"    "$execstart"
    want "ExecStart targets operator mailbox" "operator" "$execstart"
fi

echo
echo "=== Timer file: fires every 15 minutes ==="

TMR="$UNIT_DIR/spira-mail-tidy.timer"
[ -r "$TMR" ] || bad "spira-mail-tidy.timer readable" "not found at $TMR"

onactive="$(grep '^OnUnitActiveSec=' "$TMR" 2>/dev/null | head -1)"
if [ -z "$onactive" ]; then
    bad "spira-mail-tidy.timer has OnUnitActiveSec" "absent — fires once per boot only"
else
    ok "spira-mail-tidy.timer has OnUnitActiveSec"
    want "fires every 15min" "15min" "$onactive"
fi

unit_line="$(grep '^Unit=' "$TMR" 2>/dev/null | head -1 | cut -d= -f2-)"
want "timer names the tidy service" "spira-mail-tidy" "${unit_line:-}"

echo
echo "=== units.sh: timer in UNITS and _ENABLE_TMPL ==="

UNITS_SH="$UNIT_DIR/units.sh"
units_block="$(awk '/^UNITS=\(/{found=1} found{print} found && /\)/{found=0}' "$UNITS_SH")"
enable_block="$(awk '/_ENABLE_TMPL=\(/{found=1} found{print} found && /\)/{found=0}' "$UNITS_SH")"

# POSITIVE CONTROL: a known entry (spira-sentinel.timer) appears in both blocks.
case "$units_block" in
    *spira-sentinel.timer*) ok "positive control: units_block is parseable" ;;
    *) bad "positive control: units_block is parseable" "spira-sentinel.timer absent — parser broken" ;;
esac
case "$enable_block" in
    *spira-sentinel.timer*) ok "positive control: enable_block is parseable" ;;
    *) bad "positive control: enable_block is parseable" "spira-sentinel.timer absent — parser broken" ;;
esac

case "$units_block" in
    *spira-mail-tidy.timer*)   ok "spira-mail-tidy.timer is in UNITS" ;;
    *) bad "spira-mail-tidy.timer is in UNITS" "absent — install.sh will not write it" ;;
esac
case "$units_block" in
    *spira-mail-tidy.service*) ok "spira-mail-tidy.service is in UNITS" ;;
    *) bad "spira-mail-tidy.service is in UNITS" "absent — install.sh will not write it" ;;
esac
case "$enable_block" in
    *spira-mail-tidy.timer*)   ok "spira-mail-tidy.timer is in _ENABLE_TMPL" ;;
    *) bad "spira-mail-tidy.timer is in _ENABLE_TMPL" "absent — timer will not be enabled" ;;
esac

tl_summary
