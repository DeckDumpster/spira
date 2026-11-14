#!/usr/bin/env bash
#
# test-lc-backup-drill-deferral.sh — a drill that stays deferred past its wait cap exits red
# with its verdict recorded, so explain-drill reports it; no second timer slot retries it.
#
#   ./test-lc-backup-drill-deferral.sh
#
# covers: spira/lc-backup.sh systemd/lc-backup-drill.* spira/conf.d/SPIRA_LC_BACKUP_DRILL_*
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
fail=0
ok() { echo "ok - $1"; }
bad() { echo "not ok - $1: $2"; fail=1; }

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

! grep -q '16:50' "$HERE/../systemd/lc-backup-drill.timer" && ok "the timer has one slot" || bad "the timer has one slot" "16:50 present"
[ "$(grep -c '^OnCalendar=' "$HERE/../systemd/lc-backup-drill.timer")" -eq 1 ] && ok "exactly one OnCalendar" || bad "exactly one OnCalendar" "count"
exit "$fail"
