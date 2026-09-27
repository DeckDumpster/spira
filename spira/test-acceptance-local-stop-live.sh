#!/usr/bin/env bash
# test-acceptance-local-stop-live.sh — the real property behind acceptance-local.sh's
# stop <round>: stopping a systemd --user transient unit by NAME is cgroup-wide, and
# reaches a child that was reparented to whatever reaped it before the stop ran.
#
# THE INCIDENT THIS REGRESSES (sp-kb0k5). A Concierge subagent wanted to stop a leftover
# acceptance-local.sh. It read the process's PPid from /proc and killed it. The process was
# orphaned, so its PPid was the user manager itself; SIGTERM to it activated exit.target and
# the whole session — every aeon, sentinel, timer — stopped. Reproduced here for real: a
# transient unit backgrounds a child from a subshell and lets that subshell exit
# immediately, orphaning the child before this suite calls named_unit_stop. Reparenting
# changes the child's PPid, never its cgroup, so the unit's own KillMode=control-group still
# catches it — and the process that reaped it, and an unrelated unit, are never touched.
#
# HOST-ONLY, AND SAID SO. test-acceptance-local-stop.sh covers the argument wiring with
# fakes and needs no real systemd session; this file is the one dependency neither a fake
# nor a container's own lean session can stand in for.
#
# covers: spira/acceptance-local.sh spira/lib.sh
# host-reason: needs a real systemd --user session (systemd-run) to prove a unit's cgroup
#   survives a child's reparenting; a container has no such session.
# tier: T4
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"
. "$HERE/lib.sh"

if ! systemctl --user status >/dev/null 2>&1 || ! command -v systemd-run >/dev/null 2>&1; then
    skip "no systemd --user session — this is a T4 host acceptance suite, reported as skipped rather than folded into a container's green"
fi

echo "test-acceptance-local-stop-live.sh"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM

_round="stopreal$$"
_ts="$(date +%s)"
_unit="spira-acc-${_round}-${_ts}"
_other_unit="spira-acc-otherround$$-${_ts}"
_marker="$TMP/orphan-pid"

systemd-run --user --collect --quiet --unit="$_unit" -- \
    bash -c "( sleep 60 & echo \$! > '$_marker' ); sleep 60" \
    || bad "started $_unit" "systemd-run exited non-zero"
systemd-run --user --collect --quiet --unit="$_other_unit" -- sleep 60 \
    || bad "started unrelated $_other_unit" "systemd-run exited non-zero"

_orphan=""; _tries=0
while [ "$_tries" -lt 50 ]; do
    if [ -f "$_marker" ]; then
        _cand="$(cat "$_marker" 2>/dev/null)"
        if [ -n "$_cand" ] && [ -d "/proc/$_cand" ]; then _orphan="$_cand"; break; fi
    fi
    sleep 0.2; _tries=$((_tries + 1))
done
if [ -z "$_orphan" ]; then
    bad "the orphan process appeared" "never saw a live pid in $_marker after 10s"
else
    ok "the orphan process ($_orphan) is alive"
fi

_main="$(systemctl --user show -p MainPID --value "$_unit.service" 2>/dev/null)"
_reaper=""; _tries=0
while [ "$_tries" -lt 25 ]; do
    _reaper="$(awk '/^PPid:/{print $2}' "/proc/$_orphan/status" 2>/dev/null)"
    [ -n "$_reaper" ] && [ "$_reaper" != "$_main" ] && break
    sleep 0.2; _tries=$((_tries + 1))
done
# POSITIVE CONTROL for the scenario itself: without this, a fixture that never actually
# orphaned anything (the child still parented to MainPID) would pass every assertion below
# for the wrong reason — nothing here would have caught the incident.
is "the orphan really was reparented (PPid != the unit's own MainPID)" \
    1 "$([ -n "$_reaper" ] && [ "$_reaper" != "$_main" ] && echo 1 || echo 0)"
_reaper_alive_before=0; [ -n "$_reaper" ] && [ -d "/proc/$_reaper" ] && _reaper_alive_before=1
is "the process that reaped it is alive before the stop" 1 "$_reaper_alive_before"

_stop_out="$(named_unit_stop "spira-acc-${_round}-*")"; _stop_rc=$?
wantrc "named_unit_stop exits 0" "0" "$_stop_rc"
want "it names the unit it stopped" "stopped $_unit" "$_stop_out"

sleep 0.5
[ -d "/proc/$_orphan" ] \
    && bad "the orphaned child is gone after the stop" "pid $_orphan is still alive" \
    || ok "the orphaned child is gone after the stop"

# THE ASSERTION THE OLD SCRIPT WOULD HAVE FAILED: it sent SIGTERM to $_reaper directly.
[ -n "$_reaper" ] && [ -d "/proc/$_reaper" ] \
    && ok "the process that had reaped the orphan is untouched" \
    || bad "the process that had reaped the orphan is untouched" "pid ${_reaper:-?} is gone"

_other_state="$(systemctl --user is-active "$_other_unit.service" 2>/dev/null)"
is "an unrelated unit is untouched by the stop" "active" "$_other_state"

systemctl --user stop "$_other_unit.service" >/dev/null 2>&1 || true
systemctl --user stop "$_unit.service" >/dev/null 2>&1 || true

tl_summary
