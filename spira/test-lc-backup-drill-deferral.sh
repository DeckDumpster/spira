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
tl_config SPIRA_RUN="$T/run" SPIRA_LC_BACKUP_DB=alpha SPIRA_LC_BACKUP_DRILL_WAIT_SECS=0 \
    SPIRA_LC_BACKUP_DRILL_POLL_SECS=1 SPIRA_LC_STORE_PATH="" SPIRA_LC_BACKUP_DRILL_DIR="$T/drill" \
    SPIRA_LC_BACKUP_SYNC_DIR="$T/sync"
SPIRA_RUN="$T/run"
mkdir -p "$SPIRA_RUN"

# shellcheck disable=SC1091
. "$HERE/lc-backup.sh"
_lcb_drill_busy() { echo "live store did not answer a probe"; }
_lcb_report() { echo "$1" > "$T/reported"; }

out="$(cmd_drill 2>&1)"; rc=$?
[ "$rc" -eq 1 ] && ok "a deferred drill exits red" || bad "a deferred drill exits red" "rc=$rc"
v="$SPIRA_RUN/lc-backup/alpha/drill.verdict"
grep -q 'DEFERRED' "$v" 2>/dev/null && ok "the deferral is the recorded verdict" || bad "the deferral is the recorded verdict" "$(cat "$v" 2>/dev/null)"
grep -Eq '\(quiet wait [0-9]+s\)' "$v" 2>/dev/null && ok "the deferral names the seconds it waited" || bad "the deferral names the seconds it waited" "$(cat "$v" 2>/dev/null)"
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
exec 8>&-
SPIRA_LC_BACKUP_DB="alpha beta"
_lcb_lock() { mkdir -p "$SPIRA_RUN/lc-backup/$1"; echo "$SPIRA_RUN/lc-backup/$1/.lock"; }
_lcb_drill_db_locked() { echo "ok $1" > "$(_lcb_verdict "$1")"; }
rm -f "$SPIRA_RUN"/lc-backup/*/drill.verdict
exec 8>"$(_lcb_lock beta)"; flock -n 8
cmd_drill >/dev/null 2>&1; rc=$?
[ "$rc" -eq 1 ] && ok "one database deferred makes the drill red" || bad "one database deferred makes the drill red" "rc=$rc"
grep -q '^ok alpha' "$SPIRA_RUN/lc-backup/alpha/drill.verdict" 2>/dev/null && ok "a deferral leaves another database's verdict alone" || bad "a deferral leaves another database's verdict alone" "$(cat "$SPIRA_RUN/lc-backup/alpha/drill.verdict" 2>/dev/null)"
grep -q 'DEFERRED' "$SPIRA_RUN/lc-backup/beta/drill.verdict" 2>/dev/null && ok "the deferred database records DEFERRED" || bad "the deferred database records DEFERRED" "none"
exec 8>&-
timeout_h="$(sed -n 's/^TimeoutStartSec=\([0-9]*\)h$/\1/p' "$HERE/../systemd/lc-backup-drill.service")"
[ $(( ${timeout_h:-0} * 3600 )) -gt "$(sed -n "s/.*WAIT_SECS:=\([0-9]*\)}.*/\1/p" "$HERE/conf.d/SPIRA_LC_BACKUP_DRILL_WAIT_SECS")" ] && ok "the service outlives the wait cap" || bad "the service outlives the wait cap" "timeout ${timeout_h}h"

grep -q '^OnCalendar=Sun ' "$HERE/../systemd/lc-backup-drill.timer" && ok "the timer fires weekly" || bad "the timer fires weekly" "not Sun"
! grep -q '16:50' "$HERE/../systemd/lc-backup-drill.timer" && ok "the timer has one slot" || bad "the timer has one slot" "16:50 present"
[ "$(grep -c '^OnCalendar=' "$HERE/../systemd/lc-backup-drill.timer")" -eq 1 ] && ok "exactly one OnCalendar" || bad "exactly one OnCalendar" "count"

mkdir -p "$T/bin"
cat > "$T/bin/spira-lc" <<'STUB'
#!/usr/bin/env bash
echo "drill ok: stub says $STUB_SAY"
exit "$STUB_RC"
STUB
chmod +x "$T/bin/spira-lc"
stamp="$SPIRA_RUN/lc-backup/control/drill.ok"
rm -f "$stamp"
PATH="$T/bin:$PATH" STUB_RC=0 STUB_SAY=fine _lcb_drill_control >/dev/null 2>&1; rc=$?
[ "$rc" -eq 0 ] && ok "a passing control drill exits green" || bad "a passing control drill exits green" "rc=$rc"
[ -s "$stamp" ] && [ "$(cat "$stamp")" -gt 1700000000 ] && ok "a passing control drill writes the epoch stamp the watcher reads" || bad "a passing control drill writes the epoch stamp" "$(cat "$stamp" 2>/dev/null) $(cat "$T/run.out")"
echo 1700000000 > "$stamp"
PATH="$T/bin:$PATH" STUB_RC=1 STUB_SAY=broken _lcb_drill_control >/dev/null 2>&1; rc=$?
[ "$rc" -eq 1 ] && ok "a failing control drill exits red" || bad "a failing control drill exits red" "rc=$rc"
[ "$(cat "$stamp")" = "1700000000" ] && ok "a failing control drill leaves the prior stamp" || bad "a failing control drill leaves the prior stamp" "$(cat "$stamp")"

SPIRA_LC_STORE_PATH="$T/store"
_lcb_refuse_unconfigured() { return 0; }; _lcb_refuse_same_disk() { return 0; }
_lcb_run_db() { return 0; }; _lcb_run_registry() { return 0; }
cv="$SPIRA_RUN/lc-backup/control/drill.verdict"
stage() { rm -f "$stamp" "$SPIRA_RUN"/lc-backup/*/drill.ok; printf '%s\n' "$1" > "$cv"; touch -d @1700000000 "$cv"; }
stage "lc-backup: drill ok — control: sqlite-checkpoint: /x/control.db: 482 passes: errors 0; wal 0 frames 0 B"
cmd_run >"$T/run.out" 2>&1; exec 8>&-
[ "$(cat "$stamp" 2>/dev/null)" = "1700000000" ] && ok "a passing verdict without a stamp is imported at the verdict's mtime" || bad "a passing verdict is imported" "$(cat "$stamp" 2>/dev/null) $(cat "$T/run.out")"
grep -qx "lc-backup: drill: imported control's drill.ok from its passing verdict (1700000000)" "$T/run.out" && ok "the import logs the line naming store and epoch" || bad "the import logs its line" "$(cat "$T/run.out")"
av="$SPIRA_RUN/lc-backup/alpha/drill.verdict"
echo "lc-backup: drill ok — alpha: fine" > "$av"; touch -d @1700000001 "$av"; rm -f "$SPIRA_RUN/lc-backup/alpha/drill.ok"
cmd_run >"$T/run.out" 2>&1; exec 8>&-
[ "$(cat "$SPIRA_RUN/lc-backup/alpha/drill.ok" 2>/dev/null)" = "1700000001" ] && ok "a configured database's passing verdict is imported" || bad "a configured database's verdict is imported" "$(cat "$T/run.out")"
rm -f "$SPIRA_RUN/lc-backup/alpha/drill.verdict" "$SPIRA_RUN/lc-backup/alpha/drill.ok"
stage "lc-backup: drill: control: restore failed"
cmd_run >"$T/run.out" 2>&1; exec 8>&-
[ ! -e "$stamp" ] && ok "a failing verdict gets no stamp" || bad "a failing verdict gets no stamp" "stamped"
stage "lc-backup: drill: DEFERRED past 0s, never ran — x"
cmd_run >"$T/run.out" 2>&1; exec 8>&-
[ ! -e "$stamp" ] && ok "a deferred verdict gets no stamp" || bad "a deferred verdict gets no stamp" "stamped"
stage "lc-backup: drill ok — control: fine"; echo 1600000000 > "$stamp"
cmd_run >"$T/run.out" 2>&1; exec 8>&-
[ "$(cat "$stamp")" = "1600000000" ] && ok "an existing stamp is never rewritten" || bad "an existing stamp is never rewritten" "$(cat "$stamp")"
stage "lc-backup: drill ok — control: fine"
PATH="$T/bin:$PATH" STUB_RC=1 STUB_SAY=broken cmd_drill --db control >/dev/null 2>&1; rc=$?
[ "$rc" -eq 1 ] && [ "$(cat "$stamp" 2>/dev/null)" = "1700000000" ] && ok "cmd_drill imports before a failing drill and keeps the stamp" || bad "cmd_drill imports before a failing drill" "rc=$rc $(cat "$stamp" 2>/dev/null)"

tl_summary
