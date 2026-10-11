#!/usr/bin/env bash
# test-lc-backup.sh — the lifecycle store's off-box backup, its age alarm and its restore drill.
#
# The store is a real dolt database on a throwaway data directory; the destination is a real
# dolt file:// backup. Nothing here touches a live server.
#
# tier: T1
# covers: spira/lc-backup.sh spira/watchers spira/conf.d/SPIRA_LC_BACKUP_* systemd/lc-backup.service systemd/lc-backup.timer install/src/manifest.rs
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
echo "test-lc-backup.sh"

command -v dolt >/dev/null 2>&1 || skip "no dolt on PATH"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
export DOLT_ROOT_PATH="$TMP/doltroot"; mkdir -p "$DOLT_ROOT_PATH"
dolt config --global --add user.name t >/dev/null; dolt config --global --add user.email t@t >/dev/null

DATA="$TMP/data"; RUN="$TMP/run"; DEST="$TMP/dest"; mkdir -p "$DATA/spira_lifecycle" "$RUN" "$DEST"
( cd "$DATA/spira_lifecycle" && dolt init >/dev/null \
    && dolt sql -q "CREATE TABLE bead (id INT PRIMARY KEY); CREATE TABLE audit (id INT PRIMARY KEY); INSERT INTO bead VALUES (1),(2),(3); INSERT INTO audit VALUES (1);" \
    && dolt sql -q "CALL DOLT_COMMIT('-Am','seed')" >/dev/null ) || bail "fixture store"

lcb() {
    SPIRA_TOML="$(tl_layer SPIRA_RUN="$RUN" SPIRA_DOLT_DATA="$DATA" SPIRA_LC_BACKUP_URL="${URL-file://$DEST}" \
        SPIRA_LC_BACKUP_MAX_AGE=1000 SPIRA_LC_BACKUP_ALLOW_LOCAL="${LOCALOK-1}" SPIRA_LC_BACKUP_DB=spira_lifecycle)" \
        bash "$HERE/lc-backup.sh" "$@" 2>&1
}

echo; echo "refusals — no destination, same disk:"
URL="" lcb run >/dev/null; wantrc "run with no destination refuses" 1 $?
out="$(URL="" lcb age)"; rc=$?; wantrc "age with no destination is red" 1 $rc; want "age names the gap" no-destination "$out"
out="$(LOCALOK=0 lcb run)"; rc=$?; wantrc "same-disk file:// destination refused" 1 $rc; want "refusal names the exit" ALLOW_LOCAL "$out"

echo; echo "age — stale and fresh fixtures:"
out="$(lcb age)"; rc=$?; wantrc "never backed up is red" 1 $rc; want "never is named" never "$out"
printf '%s deadbeef\n' "$(( $(date +%s) - 5000 ))" > "$RUN/lc-backup.ok"
out="$(lcb age)"; rc=$?; wantrc "stale stamp is red" 1 $rc; want "stale is named" stale "$out"
printf '%s deadbeef\n' "$(( $(date +%s) - 10 ))" > "$RUN/lc-backup.ok"
out="$(lcb age)"; rc=$?; wantrc "fresh stamp is green" 0 $rc; want "fresh is named" ok "$out"
printf 'junk\n' > "$RUN/lc-backup.ok"; lcb age >/dev/null; wantrc "unreadable stamp cannot check (2)" 2 $?
rm -f "$RUN/lc-backup.ok"

echo; echo "run, then drill — uncommitted rows are carried, counts identical:"
( cd "$DATA/spira_lifecycle" && dolt sql -q "INSERT INTO bead VALUES (4)" )
out="$(lcb run)"; rc=$?; wantrc "run succeeds [$(echo $out | tr -d "\n" | cut -c1-400)]" 0 $rc; want "run reports the sync" synced "$out"
lcb age >/dev/null; wantrc "age green after run" 0 $?
out="$(lcb drill)"; rc=$?; wantrc "drill ok on identical stores" 0 $rc; want "drill reports tables" "2 table(s) identical" "$out"
( cd "$DATA/spira_lifecycle" && dolt sql -q "INSERT INTO bead VALUES (5)" && dolt sql -q "CALL DOLT_COMMIT('-Am','later')" >/dev/null )
lcb drill >/dev/null; wantrc "drill compares AS OF the backed-up commit, so later live writes are not drift" 0 $?

echo; echo "drill — positive control: a destination that disagrees is red:"
rm -rf "$DEST"/*; mkdir -p "$TMP/other" && ( cd "$TMP/other" && mkdir spira_lifecycle && cd spira_lifecycle && dolt init >/dev/null \
    && dolt sql -q "CREATE TABLE bead (id INT PRIMARY KEY); CALL DOLT_COMMIT('-Am','x')" >/dev/null && dolt sql -q "CALL DOLT_BACKUP('sync-url','file://$DEST')" >/dev/null )
out="$(lcb drill)"; rc=$?; wantrc "drill red when the destination holds another store" 1 $rc; want "drill names the head mismatch" "stamped head" "$out"

echo; echo "units and rows:"
grep -q '^ExecStart=@SPIRA_HOME@/lc-backup.sh run' "$HERE/../systemd/lc-backup.service" && ok "service runs the backup" || bad "service runs the backup"
grep -q '^ExecStartPost=@SPIRA_HOME@/lc-backup.sh drill' "$HERE/../systemd/lc-backup.service" && ok "service drills after the backup" || bad "service drills"
grep -q '^Unit=lc-backup.service' "$HERE/../systemd/lc-backup.timer" && ok "timer starts the service" || bad "timer starts the service"
grep -q 'lc-backup.timer", true' "$HERE/../install/src/manifest.rs" && ok "manifest enables the timer" || bad "manifest enables the timer"
grep -q '^lc-backup|daemon|' "$HERE/watchers" && ok "age alarm has a watcher row" || bad "age alarm has a watcher row"
tl_summary
