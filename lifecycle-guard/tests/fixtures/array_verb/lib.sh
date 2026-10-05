#!/usr/bin/env bash
set -uo pipefail
json_only() { cat; }
bdjson() { bdq "$@" --json 2>/dev/null | json_only; }
READY=(ready --limit 0)
READY+=(--label x)
bdq "${READY[@]}" --json
WRITE=(update "$1")
WRITE+=(--status open)
bdq "${WRITE[@]}"
bdjson show "$1"
bdjson close "$1"
FILLED=()
while IFS= read -r a; do FILLED+=("$a"); done < <(printf 'close\n')
bdq "${FILLED[@]}"
