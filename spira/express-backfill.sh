#!/usr/bin/env bash
# express-backfill.sh — one-time: record Express on the lifecycle row of every bead that
# still carries the express bd label. Idempotent (Express on an express row applies and
# changes nothing); safe to re-run. Prints one line per bead; non-zero if any failed.
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
LABEL="$(timeout 5 spira-config get spira.express_label)" || { echo "express-backfill: cannot read the express label" >&2; exit 2; }
# batch-job: one-time listing of every labelled bead
ids="$(timeout 60 bdq list --label "$LABEL" --all --limit 0 --json | python3 -c 'import json,sys
for b in json.load(sys.stdin): print(b["id"])')" || { echo "express-backfill: cannot list labelled beads" >&2; exit 2; }
rc=0
for id in $ids; do
    if timeout 5 spira-lc express "$id" >/dev/null 2>&1; then echo "express: $id"; else echo "FAILED: $id" >&2; rc=1; fi
done
exit "$rc"
