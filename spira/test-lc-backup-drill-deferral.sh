#!/usr/bin/env bash
#
# test-lc-backup-drill-deferral.sh — a drill that stays deferred past its wait cap exits red
# with its verdict recorded, so explain-drill reports it; no second timer slot retries it.
#
#   ./test-lc-backup-drill-deferral.sh
#
# tier: T1
# covers: spira/lc-backup.sh systemd/lc-backup-drill.* spira/conf.d/SPIRA_LC_BACKUP_DRILL_*
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT
export SPIRA_RUN="$T/run" SPIRA_LC_BACKUP_DB="alpha" SPIRA_LC_BACKUP_DRILL_WAIT_SECS=0 \
    SPIRA_LC_BACKUP_DRILL_POLL_SECS=1 SPIRA_LC_STORE_PATH=""
mkdir -p "$SPIRA_RUN"

# shellcheck disable=SC1091
. "$HERE/lc-backup.sh"
_lcb_drill_busy() { echo "live store did not answer a probe"; }
_lcb_report() { echo "$1" > "$T/reported"; }

out="$(cmd_drill 2>&1)"; rc=$?
[ "$rc" -eq 1 ] && ok "a deferred drill exits red" || bad "a deferred drill exits red" "rc=$rc"
v="$SPIRA_RUN/lc-backup/alpha/drill.verdict"
grep -q 'DEFERRED' "$v" 2>/dev/null && ok "the deferral is the recorded verdict" || bad "the deferral is the recorded verdict" "$(cat "$v" 2>/dev/null)"
SERVICE_RESULT=exit-code cmd_explain_drill 2>/dev/null
grep -q 'DEFERRED' "$T/reported" 2>/dev/null && ok "explain-drill reports the deferral" || bad "explain-drill reports the deferral" "$(cat "$T/reported" 2>/dev/null)"

rm -f "$v" "$SPIRA_RUN/lc-backup/alpha/drill.deferred"
_lcb_drill_busy() { :; }
_lcb_lock() { echo "$T/held.lock"; }
_lcb_require_db() { return 0; }
exec 8>"$T/held.lock"; flock -n 8
cmd_drill >/dev/null 2>&1; rc=$?
[ "$rc" -eq 1 ] && ok "a drill whose lock stays held exits red" || bad "a drill whose lock stays held exits red" "rc=$rc"
grep -q 'DEFERRED' "$v" 2>/dev/null && ok "a held lock records the deferral" || bad "a held lock records the deferral" "none"
timeout_h="$(sed -n 's/^TimeoutStartSec=\([0-9]*\)h$/\1/p' "$HERE/../systemd/lc-backup-drill.service")"
[ $(( ${timeout_h:-0} * 3600 )) -gt "$(sed -n "s/.*WAIT_SECS:=\([0-9]*\)}.*/\1/p" "$HERE/conf.d/SPIRA_LC_BACKUP_DRILL_WAIT_SECS")" ] && ok "the service outlives the wait cap" || bad "the service outlives the wait cap" "timeout ${timeout_h}h"

! grep -q '16:50' "$HERE/../systemd/lc-backup-drill.timer" && ok "the timer has one slot" || bad "the timer has one slot" "16:50 present"
[ "$(grep -c '^OnCalendar=' "$HERE/../systemd/lc-backup-drill.timer")" -eq 1 ] && ok "exactly one OnCalendar" || bad "exactly one OnCalendar" "count"
tl_summary
