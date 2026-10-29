#!/usr/bin/env bash
# test-lc-backup-static.sh — lc-backup's refusals, age alarm, units and watcher row. No store, no
# sleeps, no concurrent writers: the destination is never reached and age reads stamp files only.
#
# tier: T1
# covers: spira/lc-backup.sh spira/watchers systemd/lc-backup.service systemd/lc-backup.timer systemd/lc-backup-drill.* install/src/manifest.rs
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
echo "test-lc-backup-static.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
RUN="$TMP/run"; mkdir -p "$RUN"
lcb() {
    SPIRA_TOML="$(tl_layer SPIRA_RUN="$RUN" SPIRA_DOLT_DATA="$TMP/data" SPIRA_LC_BACKUP_URL="${URL-file://$TMP/dest}" \
        SPIRA_LC_BACKUP_MAX_AGE=1000 SPIRA_LC_BACKUP_ALLOW_LOCAL="${LOCALOK-1}" SPIRA_LC_BACKUP_DB="${DBS-spira_lifecycle spira}")" \
        bash "$HERE/lc-backup.sh" "$@" 2>&1
}

echo; echo "refusals — no destination, same disk:"
URL="" lcb run >/dev/null; wantrc "run with no destination refuses" 1 $?
out="$(URL="" lcb age)"; rc=$?; wantrc "age with no destination is red" 1 $rc; want "age names the gap" no-destination "$out"
out="$(URL="" lcb watch --ticks 1)"; want "watcher fires RED on no destination" "RED:" "$out"; want "watcher row records red" red "$(cat "$RUN/watchd/lc-backup.state" 2>/dev/null)"
rm -rf "$RUN/watchd"
out="$(LOCALOK=0 lcb run)"; rc=$?; wantrc "same-disk file:// destination refused" 1 $rc; want "refusal names the exit" ALLOW_LOCAL "$out"


echo; echo "age — stale and fresh fixtures:"
out="$(lcb age)"; want "never is named" never "$out"
eval "$(sed -n '/^_lcb_all_ok()/p' "$HERE/lc-backup.sh")"
declare -F _lcb_all_ok >/dev/null || bad "_lcb_all_ok was not extracted from lc-backup.sh"
_lcb_all_ok "db0: ok 1s"; wantrc "the matcher accepts an all-ok list" 0 $?
printf '%s deadbeef\n' "$(( $(date +%s) - 5000 ))" > "$RUN/lc-backup.spira_lifecycle.ok"
out="$(DBS=spira_lifecycle lcb age)"; rc=$?; wantrc "stale stamp is red" 1 $rc; want "stale is named" stale "$out"
printf '%s deadbeef\n' "$(( $(date +%s) - 10 ))" > "$RUN/lc-backup.spira_lifecycle.ok"
out="$(DBS=spira_lifecycle lcb age)"; rc=$?; wantrc "fresh stamp is green" 0 $rc; want "fresh is named" ok "$out"
printf 'junk\n' > "$RUN/lc-backup.spira_lifecycle.ok"; DBS=spira_lifecycle lcb age >/dev/null; wantrc "unreadable stamp cannot check (2)" 2 $?
rm -f "$RUN/lc-backup.spira_lifecycle.ok"
printf '%s deadbeef\n' "$(( $(date +%s) - 10 ))" > "$RUN/lc-backup.spira_lifecycle.ok"
out="$(lcb age)"; rc=$?; wantrc "one fresh database does not mask a never-backed-up one" 1 $rc; want "the missing one is named" "spira: never" "$out"
printf '%s deadbeef\n' "$(( $(date +%s) - 10 ))" > "$RUN/lc-backup.spira.ok"
lcb age >/dev/null; wantrc "both fresh is green" 0 $?
printf '%s deadbeef\n' "$(( $(date +%s) - 5000 ))" > "$RUN/lc-backup.spira.ok"
out="$(lcb age)"; rc=$?; wantrc "either stale is red" 1 $rc; want "the stale one is named" "spira: stale" "$out"
rm -f "$RUN"/lc-backup.*.ok

echo; echo "units and rows:"
grep -q '^ExecStart=@SPIRA_HOME@/lc-backup.sh run' "$HERE/../systemd/lc-backup.service" && ok "service runs the backup" || bad "service runs the backup"

! grep -q '^ExecStartPost=' "$HERE/../systemd/lc-backup.service" && ok "backup service carries no drill" || bad "backup service carries no drill"
grep -q '^ExecStart=@SPIRA_HOME@/lc-backup.sh drill' "$HERE/../systemd/lc-backup-drill.service" && ok "drill is its own unit" || bad "drill is its own unit"
grep -q '^IOWeight=' "$HERE/../systemd/lc-backup-drill.service" && ok "drill unit is I/O weighted" || bad "drill unit is I/O weighted"
grep -q '^Unit=lc-backup-drill.service' "$HERE/../systemd/lc-backup-drill.timer" && grep -q 'lc-backup-drill.timer", true' "$HERE/../install/src/manifest.rs" && ok "drill has its own enabled timer" || bad "drill has its own enabled timer"
grep -q '^X-Spira-NoDrain=true' "$HERE/../systemd/lc-backup-drill.service" && ok "drill opts out of the activation drain" || bad "drill opts out of the activation drain"
! grep -q '^X-Spira-NoDrain' "$HERE/../systemd/lc-backup.service" && ok "backup run still drains" || bad "backup run still drains"
grep -q '^ExecStopPost=@SPIRA_HOME@/lc-backup.sh explain-drill' "$HERE/../systemd/lc-backup-drill.service" && ok "drill failure is explained on stop" || bad "drill failure is explained on stop"
grep -q '^Unit=lc-backup.service' "$HERE/../systemd/lc-backup.timer" && ok "timer starts the service" || bad "timer starts the service"
grep -q 'lc-backup.timer", true' "$HERE/../install/src/manifest.rs" && ok "manifest enables the timer" || bad "manifest enables the timer"
grep -q '^lc-backup|daemon|' "$HERE/watchers" && ok "age alarm has a watcher row" || bad "age alarm has a watcher row"
