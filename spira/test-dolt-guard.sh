#!/usr/bin/env bash
# test-dolt-guard.sh — a watcher pass never signals dolt while a declared maintenance operation runs.
#
# tier: T1
# covers: spira/dolt-guard.sh spira/lc-backup.sh spira/conf.d/SPIRA_DOLT_MAINTENANCE_UNITS systemd/lc-backup.service
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
echo "test-dolt-guard.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
RUN="$TMP/run"; mkdir -p "$RUN/maintenance" "$RUN/lc-backup/db" "$TMP/bin"
printf '#!/bin/sh\necho "${FAKE_UNIT_STATE:-inactive}"\n[ "${FAKE_UNIT_STATE:-inactive}" = active ]\n' > "$TMP/bin/systemctl"; chmod +x "$TMP/bin/systemctl"
printf '#!/bin/sh\necho x >> %s/signalled\n' "$TMP" > "$TMP/bin/fakekill"; chmod +x "$TMP/bin/fakekill"
guard() {
    PATH="$TMP/bin:$PATH" SPIRA_TOML="$(tl_layer SPIRA_RUN="$RUN" SPIRA_DOLT_MAINTENANCE_UNITS="my-maint.service")" \
        bash "$HERE/dolt-guard.sh" "$@" 2>&1
}
signalled() { [ -s "$TMP/signalled" ] && wc -l < "$TMP/signalled" || echo 0; }

echo; echo "no operation running — it signals and says what it judged:"
out="$(guard signal --reason "3 queries >300s (pid 42)" -- fakekill)"; rc=$?
wantrc "signal exits 0" 0 $rc; want "judgement is logged" "3 queries >300s (pid 42)" "$out"; want "signalled once" 1 "$(signalled)"

echo; echo "lc-backup's lock held — it defers (seen to fail first without the lock above):"
rm -f "$TMP/signalled"
exec 7>"$RUN/lc-backup/db/.lock"; flock -n 7
out="$(guard signal --reason "runaway" -- fakekill)"
want "defers naming the operation" "deferring: lc-backup running" "$out"; want "did not signal" 0 "$(signalled)"
exec 7>&-
out="$(guard signal --reason "runaway" -- fakekill)"; want "lock released, signals again" 1 "$(signalled)"

echo; echo "a maintenance lock and a maintenance unit also defer:"
rm -f "$TMP/signalled"
exec 7>"$RUN/maintenance/dolt-persist-compact.lock"; flock -n 7
out="$(guard signal --reason r -- fakekill)"; want "named lock defers" "deferring: dolt-persist-compact running" "$out"; exec 7>&-
out="$(FAKE_UNIT_STATE=active guard signal --reason r -- fakekill)"; want "active unit defers" "deferring: my-maint running" "$out"
want "nothing signalled" 0 "$(signalled)"

echo; echo "a reason is required:"
guard signal -- fakekill >/dev/null; wantrc "no reason refused" 2 $?

echo; echo "lc-backup explain puts the last error line on stderr:"
mkdir -p "$RUN"; printf 'noise\nlc-backup: spira: local snapshot failed: invalid connection\ntrailing\n' > "$RUN/lc-backup.log"
out="$(SERVICE_RESULT=exit-code PATH="$TMP/bin:$PATH" SPIRA_TOML="$(tl_layer SPIRA_RUN="$RUN")" bash "$HERE/lc-backup.sh" explain 2>&1 >/dev/null)"
want "last error line echoed" "invalid connection" "$out"
