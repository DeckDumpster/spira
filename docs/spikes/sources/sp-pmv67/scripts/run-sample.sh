#!/usr/bin/env bash
# run-sample.sh <repo> <outdir> < suite-list — run each suite in a minimal environment.
set -uo pipefail
REPO="$1"; OUT="$2"; mkdir -p "$OUT"
while read -r s; do
    t0=$(date +%s%N)
    env -i HOME="$HOME" PATH="$PATH" USER="$USER" LANG=C.UTF-8 \
        TESTDB_SHARED="$TESTDB_SHARED" TESTDB_NAME="$TESTDB_NAME" TESTDB_DIR="$TESTDB_DIR" \
        TESTDB_BASELINE="$TESTDB_BASELINE" TESTDB_MODE="$TESTDB_MODE" TESTDB_BIN="$TESTDB_BIN" \
        timeout 120 bash "$REPO/spira/$s" > "$OUT/$s.out" 2>&1
    rc=$?
    printf '%s\t%s\t%s\n' "$s" "$rc" "$(( ($(date +%s%N)-t0)/1000000 ))" >> "$OUT/runs.tsv"
done
