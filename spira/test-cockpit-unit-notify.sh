#!/usr/bin/env bash
# test-cockpit-unit-notify.sh — watchdog configuration for the cockpit collector.
#
# TWO PROPERTIES, each requiring a positive control:
#
#   1. NotifyAccess=all: every service unit that carries WatchdogSec must also
#      declare NotifyAccess=all, because the heartbeat is sent via systemd-notify
#      (a child process), which `main` rejects.
#      Positive control: a unit with WatchdogSec but without NotifyAccess=all
#      must be flagged — ensures the check can detect the defect, not just find
#      nothing to do.
#
#   2. Early ping: _supervisor_loop in collect.sh must send a watchdog ping before
#      any setup code (the _write_never_frag loop). Without it, a slow startup
#      exhausts WatchdogSec before the first in-loop ping fires.
#      Positive control: a loop-only variant (ping inside while, not before setup)
#      must be detected as arriving too late.
#
# covers: systemd/*.service spira/collect.sh
set -uo pipefail

HERE="$(cd "$(dirname "$0")" && pwd -P)"
COLLECT="$HERE/collect.sh"

pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }

echo "test-cockpit-unit-notify.sh"

# ── Property 1: NotifyAccess=all on all watchdog units ────────────────────────
checked=0
for u in "$HERE"/../systemd/*.service; do
    body="$(grep -v '^\s*#' "$u")"
    case "$body" in *WatchdogSec=*) ;; *) continue ;; esac
    checked=$((checked + 1))
    case "$body" in
        *NotifyAccess=all*)
            ok "$(basename "$u"): NotifyAccess=all" ;;
        *)
            bad "$(basename "$u"): NotifyAccess=all" "missing — systemd-notify pings need NotifyAccess=all" ;;
    esac
done

# Positive control: a fabricated unit with WatchdogSec but no NotifyAccess=all fails.
BAD_UNIT='[Service]
WatchdogSec=30
ExecStart=/bin/true'
case "$BAD_UNIT" in *NotifyAccess=all*) bad "pc NotifyAccess: bad unit is detected" "not detected" ;;
    *) ok "pc NotifyAccess: bad unit is detected" ;; esac

[ "$checked" -gt 0 ] && ok "at least one watchdog unit was examined" \
    || bad "at least one watchdog unit was examined" "none found — is WatchdogSec missing?"

# ── Property 2: early ping precedes initialization in _supervisor_loop ────────
FUNC_BODY=$(awk '/^_supervisor_loop\(\)/{ p=1 } p{ print } p && /^\}$/{ exit }' "$COLLECT")
if [ -z "$FUNC_BODY" ]; then
    bad "_supervisor_loop found in collect.sh" "function not found"
else
    ok "_supervisor_loop found in collect.sh"

    PING_LINE=$(printf '%s\n' "$FUNC_BODY" | grep -n 'systemd-notify.*--watchdog' | head -1 | cut -d: -f1)
    SETUP_LINE=$(printf '%s\n' "$FUNC_BODY" | grep -n '_write_never_frag' | head -1 | cut -d: -f1)

    if [ -z "$PING_LINE" ]; then
        bad "early ping precedes init" "no systemd-notify --watchdog found in _supervisor_loop"
    elif [ -z "$SETUP_LINE" ]; then
        bad "early ping precedes init" "_write_never_frag not found in _supervisor_loop"
    elif [ "$PING_LINE" -lt "$SETUP_LINE" ]; then
        ok "early ping precedes init (line $PING_LINE < $SETUP_LINE)"
    else
        bad "early ping precedes init" "ping at line $PING_LINE, init at line $SETUP_LINE — ping arrives too late"
    fi

    # Positive control: a loop-only variant (ping only inside while, after init) is detected.
    LOOP_ONLY='_supervisor_loop() {
    for name in a b; do _write_never_frag "$name"; done
    while :; do
        [ -n "${NOTIFY_SOCKET:-}" ] && systemd-notify --watchdog || true
        sleep 5
    done
}'
    LO_PING=$(printf '%s\n' "$LOOP_ONLY" | grep -n 'systemd-notify.*--watchdog' | head -1 | cut -d: -f1)
    LO_SETUP=$(printf '%s\n' "$LOOP_ONLY" | grep -n '_write_never_frag' | head -1 | cut -d: -f1)
    if [ -n "$LO_PING" ] && [ -n "$LO_SETUP" ] && [ "$LO_PING" -gt "$LO_SETUP" ]; then
        ok "pc early ping: loop-only variant is detected as too late"
    else
        bad "pc early ping: loop-only variant is detected as too late" \
            "PING=$LO_PING SETUP=$LO_SETUP — positive control broken"
    fi
fi

echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
