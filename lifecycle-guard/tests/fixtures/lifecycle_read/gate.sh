#!/usr/bin/env bash
set -uo pipefail

id="$1"
status="$(bd show "$id" --field status)"
if [ "$status" = "closed" ]; then
    echo "already closed"
fi
