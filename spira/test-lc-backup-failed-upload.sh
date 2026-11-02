#!/usr/bin/env bash
# test-lc-backup-failed-upload.sh — a run whose upload fails leaves the drill's copy at the last
# uploaded snapshot, so the next drill restores what the stamp names and passes; the age check
# then reports the stamp's real age (sp-t08b53: a drill read a never-uploaded pack as a corrupt backup).
#
# tier: T1
# covers: spira/lc-backup.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
echo "test-lc-backup-failed-upload.sh"
command -v dolt >/dev/null 2>&1 || { echo "FAIL: dolt is not on PATH"; exit 1; }
# batch-job: fixture dolt calls against the suite's own throwaway store; a gc of it outlasts 5 s
dt() { timeout 120 dolt "$@"; }

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
export DOLT_ROOT_PATH="$TMP/doltroot"; mkdir -p "$DOLT_ROOT_PATH"
dt config --global --add user.name test >/dev/null && dt config --global --add user.email test@spira >/dev/null
RUN="$TMP/run" DATA="$TMP/data"; mkdir -p "$RUN" "$DATA/d0"
( cd "$DATA/d0" && dt init >/dev/null \
  && dt sql -q "CREATE TABLE t (id INT PRIMARY KEY); INSERT INTO t VALUES (1),(2)" \
  && dt commit -Am one >/dev/null ) || { echo "FAIL: cannot build the fixture store"; exit 1; }

lcb() {
    SPIRA_TOML="$(tl_layer SPIRA_RUN="$RUN" SPIRA_DOLT_DATA="$DATA" SPIRA_LC_BACKUP_URL="file://$TMP/dest" \
        SPIRA_LC_BACKUP_ALLOW_LOCAL=1 SPIRA_LC_BACKUP_DB="d0" SPIRA_LC_BACKUP_DRILL_DIR="$TMP/scratch")" \
        bash "$HERE/lc-backup.sh" "$@" 2>&1
}

out="$(lcb run)"; wantrc "the first run uploads" 0 $?
read -r _ first _ < "$RUN/lc-backup.d0.ok"

( cd "$DATA/d0" && dt sql -q "INSERT INTO t VALUES (3)" && dt commit -Am two >/dev/null )
rm -rf "$TMP/dest" && : > "$TMP/dest"
out="$(lcb run)"; rc=$?
[ "$rc" -ne 0 ]; wantrc "a run whose upload fails exits non-zero" 0 $?
want "the failure names the upload" "upload of" "$out"
read -r _ after _ < "$RUN/lc-backup.d0.ok"
want "the stamp stays at the last upload" "$first" "$after"
nowant "the never-uploaded pack is discarded" "pending" "$(ls "$RUN/lc-backup/d0")"

out="$(lcb drill --db d0)"; rc=$?
wantrc "the drill restores the snapshot the stamp names" 0 $rc
want "the drill verdict is ok" "drill ok" "$(cat "$RUN/lc-backup/d0/drill.verdict" 2>/dev/null)"

tl_summary
