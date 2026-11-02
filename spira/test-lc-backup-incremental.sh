#!/usr/bin/env bash
# test-lc-backup-incremental.sh — a run uploads only the table files no earlier run uploaded, plus
# one new manifest; nothing is packed or re-sent whole; the drill restores the newest manifest's
# file set at the stamped head.
#
# tier: T1
# covers: spira/lc-backup.sh spira/conf.d/SPIRA_LC_BACKUP_SYNC_DIR spira-lc/src/durability.rs state-store-sqlite/src/durability/dolt.rs
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
echo "test-lc-backup-incremental.sh"
command -v dolt >/dev/null 2>&1 || { echo "FAIL: dolt is not on PATH"; exit 1; }
command -v spira-lc >/dev/null 2>&1 || { echo "FAIL: spira-lc is not on PATH"; exit 1; }
# batch-job: fixture dolt calls against the suite's own throwaway store
dt() { timeout 120 dolt "$@"; }

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
export DOLT_ROOT_PATH="$TMP/doltroot"; mkdir -p "$DOLT_ROOT_PATH"
dt config --global --add user.name test >/dev/null && dt config --global --add user.email test@spira >/dev/null
RUN="$TMP/run" DATA="$TMP/data"; mkdir -p "$RUN" "$DATA/d0"
( cd "$DATA/d0" && dt init >/dev/null \
  && dt sql -q "CREATE TABLE t (id INT PRIMARY KEY, v VARCHAR(16)); INSERT INTO t VALUES (1,'a'),(2,'b')" \
  && dt commit -Am one >/dev/null ) || { echo "FAIL: cannot build the fixture store"; exit 1; }

lcb() {
    SPIRA_TOML="$(tl_layer SPIRA_RUN="$RUN" SPIRA_DOLT_DATA="$DATA" SPIRA_LC_BACKUP_URL="file://$TMP/dest" \
        SPIRA_LC_BACKUP_ALLOW_LOCAL=1 SPIRA_LC_BACKUP_DB="d0" SPIRA_LC_BACKUP_DRILL_DIR="$TMP/scratch" \
        SPIRA_LC_BACKUP_SYNC_DIR="${SYNC_DIR-$TMP/sync}")" \
        bash "$HERE/lc-backup.sh" "$@" 2>&1
}
inodes() { ( cd "$TMP/dest/d0/files" 2>/dev/null && stat -c '%n %i' * | sort ); }

out="$(SYNC_DIR="" lcb run)"; rc=$?
[ "$rc" -ne 0 ]; wantrc "a run with no local backup dir named refuses" 0 $?
want "the refusal names the key" "SPIRA_LC_BACKUP_SYNC_DIR is empty" "$out"

out="$(lcb run)"; wantrc "the first run uploads" 0 $?
first_files="$(inodes)"
[ -n "$first_files" ]; wantrc "the first run uploads the store's table files" 0 $?
want "the first run uploads one manifest" "1" "$(ls "$TMP/dest/d0/manifests" 2>/dev/null | wc -l | tr -d ' ')"

( cd "$DATA/d0" && dt sql -q "INSERT INTO t VALUES (3,'c')" && dt commit -Am two >/dev/null )
head="$( cd "$DATA/d0" && dt sql -r csv -q "SELECT commit_hash FROM dolt_log LIMIT 1" | tail -n +2 )"
out="$(lcb run)"; wantrc "the second run uploads" 0 $?
want "the second run reports what it uploaded" "uploaded d0 at $head" "$out"
want "the second run adds exactly one manifest, keyed by the head" "1" "$(ls "$TMP/dest/d0/manifests" | grep -c -- "-$head\$")"
want "two runs, two manifests" "2" "$(ls "$TMP/dest/d0/manifests" | wc -l | tr -d ' ')"
second_files="$(inodes)"
unchanged="$(comm -12 <(printf '%s\n' "$first_files") <(printf '%s\n' "$second_files") | wc -l | tr -d ' ')"
want "no file the first run uploaded is sent again" "$(printf '%s\n' "$first_files" | wc -l | tr -d ' ')" "$unchanged"
[ "$(printf '%s\n' "$second_files" | wc -l)" -gt "$(printf '%s\n' "$first_files" | wc -l)" ]
wantrc "the commit's new table files are uploaded" 0 $?
want "nothing is packed" "" "$(find "$TMP/dest" "$RUN" -name '*.tar*' 2>/dev/null)"
read -r _ stamped _ < "$RUN/lc-backup.d0.ok" 2>/dev/null
want "the stamp names the store's head" "$head" "${stamped:-}"

out="$(lcb drill --db d0)"; rc=$?
wantrc "the drill restores the newest manifest's file set" 0 $rc
want "the drill verdict is ok at the stamped head" "drill ok — d0: 1 table(s) identical at $head" "$(cat "$RUN/lc-backup/d0/drill.verdict" 2>/dev/null)"

tl_summary
