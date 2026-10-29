#!/usr/bin/env bash
# test-lc-backup-drill.sh — a drill that the busy store starves past its cap ends as a recorded
# deferral (exit 0, verdict file), never as a failed unit with no verdict.
#
# tier: T1
# covers: spira/lc-backup.sh systemd/lc-backup-drill.*
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
echo "test-lc-backup-drill.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
RUN="$TMP/run"; mkdir -p "$RUN"
lcb() {
    SPIRA_TOML="$(tl_layer SPIRA_RUN="$RUN" SPIRA_LC_BACKUP_URL="file://$TMP/dest" SPIRA_LC_BACKUP_ALLOW_LOCAL=1 \
        SPIRA_LC_BACKUP_DB="d0" SPIRA_LC_BACKUP_DRILL_WAIT_SECS=0 SPIRA_LC_BACKUP_DRILL_POLL_SECS=1 \
        SPIRA_LC_BACKUP_DRILL_DIR="$TMP/scratch")" \
        bash "$HERE/lc-backup.sh" "$@" 2>&1
}

out="$(lcb drill)"; rc=$?
wantrc "a drill deferred past its cap exits 0, not a unit failure" 0 $rc
want "the deferral is reported" "DEFERRED past 0s, never ran" "$out"
mkdir -p "$RUN/lc-backup/d0"; echo "lc-backup: drill ok — d0: 3 table(s) identical at abc" > "$RUN/lc-backup/d0/drill.verdict"
out="$(lcb drill)"
v="$RUN/lc-backup/d0/drill.deferred"
[ -s "$v" ]; wantrc "the deferral is recorded" 0 $?
want "the verdict says deferred" "DEFERRED" "$(cat "$v" 2>/dev/null)"

want "the last proven restore is not overwritten by the deferral" "drill ok" "$(cat "$RUN/lc-backup/d0/drill.verdict")"

timer="$HERE/../systemd/lc-backup-drill.timer"
[ "$(grep -c '^OnCalendar=' "$timer")" -ge 2 ]; wantrc "the timer has a second slot that re-arms a deferred drill" 0 $?
service="$HERE/../systemd/lc-backup-drill.service"
nowant "the oneshot unit carries no Restart= policy" "Restart" "$(cat "$service")"

tl_summary
