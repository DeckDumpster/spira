#!/usr/bin/env bash
set -uo pipefail

mark_it() {
    land_mark "$1" LANDED "$2"
}

cat "$LANDSTATE/sp-1"
