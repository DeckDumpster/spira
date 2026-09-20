#!/usr/bin/env bash
# test-cockpit-unit-notify.sh — fence: no unit template whose ExecStart runs bash may carry
# WatchdogSec=. bash cannot satisfy WatchdogSec — only the main PID's own datagram is
# credited, and an unprivileged process cannot assert another PID in SCM_CREDENTIALS.
# Lift this fence when sp-mplcb lands (Rust supervisor that is the main PID).
#
# covers: systemd/*.service spira/collect.sh
set -uo pipefail

HERE="$(cd "$(dirname "$0")" && pwd -P)"

pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }

echo "test-cockpit-unit-notify.sh"

# ── Fence: no bash ExecStart unit carries WatchdogSec= ────────────────────────
checked=0
for u in "$HERE"/../systemd/*.service; do
    body="$(grep -v '^\s*#' "$u")"
    case "$body" in *'ExecStart='*bash*|*'ExecStart='*.sh*) ;; *) continue ;; esac
    checked=$((checked + 1))
    case "$body" in
        *WatchdogSec=*)
            bad "$(basename "$u"): no WatchdogSec on bash unit" \
                "bash cannot deliver watchdog heartbeats — see sp-3az9w, sp-mplcb" ;;
        *)
            ok "$(basename "$u"): no WatchdogSec on bash unit" ;;
    esac
done

# Positive control: a fixture with bash ExecStart and WatchdogSec is detected.
BAD_UNIT='[Service]
ExecStart=/usr/bin/bash /some/script.sh
WatchdogSec=60'
_pc_found=0
_pc_body="$(printf '%s' "$BAD_UNIT" | grep -v '^\s*#')"
case "$_pc_body" in *'ExecStart='*bash*|*'ExecStart='*.sh*)
    case "$_pc_body" in *WatchdogSec=*) _pc_found=1 ;; esac ;;
esac
[ "$_pc_found" -eq 1 ] \
    && ok "pc: bash+WatchdogSec fixture is detected" \
    || bad "pc: bash+WatchdogSec fixture is detected" "positive control broken"

[ "$checked" -gt 0 ] \
    && ok "at least one bash unit was examined" \
    || bad "at least one bash unit was examined" "none found — are ExecStart patterns still .sh or bash?"

echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
