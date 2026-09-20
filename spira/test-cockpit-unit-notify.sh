#!/usr/bin/env bash
# test-cockpit-unit-notify.sh — fence: no unit template whose ExecStart BINARY is bash or
# a shell script may carry WatchdogSec=.  Only the main PID's own datagram is credited by
# the manager; bash cannot satisfy that constraint (sp-3az9w).
#
# sp-mplcb wires spira-supervise (a Rust binary) as the ExecStart for spira-cockpit.service,
# so its ExecStart target is not a shell script.  This fence checks the target only, not
# arguments — the supervisor passes collect.sh as a child argument, not as the process
# being exec'd and tracked by systemd.
#
# covers: systemd/*.service spira/collect.sh supervise/**
set -uo pipefail

HERE="$(cd "$(dirname "$0")" && pwd -P)"

pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }

echo "test-cockpit-unit-notify.sh"

# ── Fence: no bash/shell ExecStart target carries WatchdogSec= ───────────────
checked=0
for u in "$HERE"/../systemd/*.service; do
    body="$(grep -v '^\s*#' "$u")"

    # Extract the ExecStart TARGET (first word only; strip optional leading - prefix).
    # Arguments passed to ExecStart may include .sh files — that is not what the fence
    # guards against.  Only the binary being exec'd matters for PID attribution.
    _target=""
    while IFS= read -r _line; do
        case "$_line" in
            ExecStart=*)
                _target="${_line#ExecStart=}"
                _target="${_target#-}"
                _target="${_target%% *}"
                break ;;
        esac
    done <<< "$body"

    case "$_target" in *bash*|*.sh) ;; *) continue ;; esac
    checked=$((checked + 1))
    case "$body" in
        *WatchdogSec=*)
            bad "$(basename "$u"): bash ExecStart target has WatchdogSec" \
                "bash cannot deliver watchdog heartbeats — see sp-3az9w" ;;
        *)
            ok "$(basename "$u"): bash ExecStart target has no WatchdogSec" ;;
    esac
done

# ── Positive control: bash ExecStart + WatchdogSec is detected ───────────────
BAD_UNIT='[Service]
ExecStart=/usr/bin/bash /some/script.sh
WatchdogSec=60'
_pc_target=""
_pc_body="$(printf '%s' "$BAD_UNIT" | grep -v '^\s*#')"
while IFS= read -r _line; do
    case "$_line" in
        ExecStart=*)
            _pc_target="${_line#ExecStart=}"
            _pc_target="${_pc_target#-}"
            _pc_target="${_pc_target%% *}"
            break ;;
    esac
done <<< "$_pc_body"
_pc_found=0
case "$_pc_target" in *bash*|*.sh)
    case "$_pc_body" in *WatchdogSec=*) _pc_found=1 ;; esac ;;
esac
[ "$_pc_found" -eq 1 ] \
    && ok "pc: bash+WatchdogSec fixture is detected" \
    || bad "pc: bash+WatchdogSec fixture is detected" "positive control broken"

# ── Positive control: Rust binary ExecStart with .sh argument is NOT flagged ─
# The supervisor passes collect.sh as an argument; the ExecStart target is not a shell script.
SUPER_UNIT='[Service]
ExecStart=/usr/local/bin/spira-supervise /path/collect.sh loop
WatchdogSec=60'
_su_target=""
while IFS= read -r _line; do
    case "$_line" in
        ExecStart=*)
            _su_target="${_line#ExecStart=}"
            _su_target="${_su_target#-}"
            _su_target="${_su_target%% *}"
            break ;;
    esac
done <<< "$SUPER_UNIT"
_su_flagged=0
case "$_su_target" in *bash*|*.sh) _su_flagged=1 ;; esac
[ "$_su_flagged" -eq 0 ] \
    && ok "pc: Rust supervisor with .sh argument is not flagged" \
    || bad "pc: Rust supervisor with .sh argument is not flagged" "positive control broken"

# ── Positive control: at least one bash unit was examined ────────────────────
[ "$checked" -gt 0 ] \
    && ok "at least one bash unit was examined" \
    || bad "at least one bash unit was examined" "none found — are ExecStart patterns still .sh or bash?"

# ── spira-cockpit.service must have WatchdogSec (supervisor wired) ───────────
_cockpit="$HERE/../systemd/spira-cockpit.service"
if [ -f "$_cockpit" ]; then
    case "$(grep -v '^\s*#' "$_cockpit")" in
        *WatchdogSec=*)
            ok "spira-cockpit.service: WatchdogSec present (Rust supervisor is main PID)" ;;
        *)
            bad "spira-cockpit.service: WatchdogSec present" \
                "sp-mplcb must wire WatchdogSec via spira-supervise" ;;
    esac
fi

echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
