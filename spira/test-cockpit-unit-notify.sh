#!/usr/bin/env bash
# test-cockpit-unit-notify.sh — a unit whose loop pings the watchdog through systemd-notify must
# accept notifications from children, or the watchdog kills every pass.
# covers: systemd/*.service spira/collect.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }

echo "test-cockpit-unit-notify.sh"
checked=0
for u in "$HERE"/../systemd/*.service; do
    body="$(grep -v '^\s*#' "$u")"
    case "$body" in *WatchdogSec=*) ;; *) continue ;; esac
    exe="$(printf '%s\n' "$body" | sed -n 's/^ExecStart=@[A-Z_]*@\/\([^ ]*\).*/\1/p' | head -1)"
    script="$HERE/$exe"
    [ -f "$script" ] || { bad "$(basename "$u") names a script this suite can read" "$exe"; continue; }
    checked=$((checked+1))
    if grep -v '^\s*#' "$script" | grep -F 'systemd-notify' >/dev/null; then
        case "$body" in
            *NotifyAccess=all*) ok "$(basename "$u"): heartbeat from systemd-notify is accepted" ;;
            *) bad "$(basename "$u"): heartbeat from systemd-notify is accepted" "needs NotifyAccess=all" ;;
        esac
    fi
done
[ "$checked" -gt 0 ] && ok "at least one watchdog unit was examined" || bad "at least one watchdog unit was examined" "none found"

echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
