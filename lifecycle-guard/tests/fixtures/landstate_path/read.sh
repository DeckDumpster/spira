#!/usr/bin/env bash
set -uo pipefail

for f in "$LANDSTATE/"*; do
    echo "$f"
done

cat "$LANDSTATE/sp-1"

read -r state _ < "$LANDSTATE/sp-1"
