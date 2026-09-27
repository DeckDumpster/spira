#!/usr/bin/env bash
#
# test-timer-restart-safety.sh — every relative-form systemd/*.timer template also
# carries OnActiveSec, so it survives a systemd --user manager restart. OnBootSec alone
# is boot-relative: after a manager restart (not a reboot) its deadline is already in
# the past and OnUnitActiveSec has no in-run activation to measure from, so the timer
# goes active (elapsed), Trigger: n/a, and never fires again (sp-ly6l9). OnCalendar
# timers are wall-clock and immune, so they are exempt.
#
# tier: T0
# covers: systemd/*.timer
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
UNIT_DIR="$HERE/../systemd"

echo
echo "=== positive control: an offending fixture is caught ==="

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

cat > "$TMP/spira-offender.timer" <<'EOF'
[Unit]
Description=A timer with no restart-safe reference point

[Timer]
OnBootSec=5min
OnUnitActiveSec=15min
AccuracySec=2min
Unit=spira-offender.service

[Install]
WantedBy=timers.target
EOF

check_timer() {  # <file> -> prints FAIL if relative-form and missing OnActiveSec
    local f="$1"
    grep -q '^OnCalendar=' "$f" && return 0
    grep -q '^OnBootSec=\|^OnUnitActiveSec=' "$f" || return 0
    grep -q '^OnActiveSec=' "$f" && return 0
    echo "FAIL"
}

result="$(check_timer "$TMP/spira-offender.timer")"
if [ "$result" = "FAIL" ]; then
    ok "positive control: offending fixture (no OnActiveSec) is caught"
else
    bad "positive control: offending fixture (no OnActiveSec) is caught" \
        "matcher found nothing — it cannot be trusted to find a real offender"
fi

echo
echo "=== every relative-form timer template carries OnActiveSec ==="

n=0
for f in "$UNIT_DIR"/*.timer; do
    [ -e "$f" ] || continue
    n=$((n+1))
    b="$(basename "$f")"
    if [ "$(check_timer "$f")" = "FAIL" ]; then
        bad "$b: has OnActiveSec (or OnCalendar)" \
            "OnBootSec/OnUnitActiveSec present with neither OnCalendar nor OnActiveSec — dead after a user-manager restart"
    else
        ok "$b: has OnActiveSec (or OnCalendar)"
    fi
done

[ "$n" -gt 0 ] || bad "found timer templates to check" "no *.timer files under $UNIT_DIR"

tl_summary
