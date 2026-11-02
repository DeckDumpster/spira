#!/usr/bin/env bash
# test-lc-backup-compact.sh — a run uploads a garbage-collected snapshot: smaller than the raw
# chunk store when its table files carry unreachable chunks (a gc that kept them live, then a
# reset that orphaned them, as on the bd store), at the same head, and the drill restores it.
#
# tier: T1
# covers: spira/lc-backup.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
echo "test-lc-backup-compact.sh"
command -v dolt >/dev/null 2>&1 || { echo "FAIL: dolt is not on PATH"; exit 1; }
# batch-job: fixture dolt calls against the suite's own throwaway store; a gc of it outlasts 5 s
dt() { timeout 120 dolt "$@"; }

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
export DOLT_ROOT_PATH="$TMP/doltroot"; mkdir -p "$DOLT_ROOT_PATH"
dt config --global --add user.name test >/dev/null && dt config --global --add user.email test@spira >/dev/null
RUN="$TMP/run" DATA="$TMP/data"; mkdir -p "$RUN" "$DATA/d0"
( cd "$DATA/d0" && dt init >/dev/null \
  && dt sql -q "CREATE TABLE keep (id INT PRIMARY KEY, v VARCHAR(16)); INSERT INTO keep VALUES (1,'a'),(2,'b')" \
  && dt commit -Am keep >/dev/null \
  && dt sql -q "CREATE TABLE junk (id INT PRIMARY KEY, v LONGTEXT)" \
  && for i in $(seq 1 20); do dt sql -q "INSERT INTO junk VALUES ($i, '$(head -c 32768 /dev/urandom | od -An -tx1 | tr -d ' \n')')" || exit 1; done \
  && dt commit -Am junk >/dev/null && dt gc >/dev/null 2>&1 \
  && dt reset --hard HEAD~1 >/dev/null ) || { echo "FAIL: cannot build the fixture store"; exit 1; }
head="$( cd "$DATA/d0" && dt sql -r csv -q "SELECT commit_hash FROM dolt_log LIMIT 1" | tail -n +2 )"
mkdir -p "$TMP/raw"
if command -v zstd >/dev/null 2>&1; then ext=zst; else ext=gz; fi
( cd "$DATA/d0" && dt sql -q "CALL DOLT_BACKUP('sync-url', 'file://$TMP/raw')" >/dev/null ) && tar -C "$TMP/raw" -caf "$TMP/raw.tar.$ext" . \
    || { echo "FAIL: cannot pack the raw baseline"; exit 1; }
raw=$(stat -c %s "$TMP/raw.tar.$ext")

lcb() {
    SPIRA_TOML="$(tl_layer SPIRA_RUN="$RUN" SPIRA_DOLT_DATA="$DATA" SPIRA_LC_BACKUP_URL="file://$TMP/dest" \
        SPIRA_LC_BACKUP_ALLOW_LOCAL=1 SPIRA_LC_BACKUP_DB="d0" SPIRA_LC_BACKUP_DRILL_DIR="$TMP/scratch")" \
        bash "$HERE/lc-backup.sh" "$@" 2>&1
}

out="$(lcb run)"; rc=$?
wantrc "the run uploads" 0 $rc
up="$(ls "$TMP/dest/d0/"*.tar.* 2>/dev/null | head -1)"
got=$(stat -c %s "$up" 2>/dev/null || echo 0)
[ -n "$up" ] && [ "$got" -lt $(( raw / 2 )) ]; wantrc "the uploaded snapshot ($got bytes) is under half the raw chunk store ($raw bytes)" 0 $?
read -r _ stamped _ < "$RUN/lc-backup.d0.ok" 2>/dev/null
want "the stamp names the store's head" "$head" "${stamped:-}"
[ ! -e "$RUN/lc-backup/d0/compact" ] && [ ! -e "$RUN/lc-backup/d0/stage" ]; wantrc "the run leaves no compaction scratch behind" 0 $?

out="$(lcb drill --db d0)"; rc=$?
wantrc "the drill restores the compacted snapshot" 0 $rc
want "the drill verdict is ok" "drill ok" "$(cat "$RUN/lc-backup/d0/drill.verdict" 2>/dev/null)"

tl_summary
