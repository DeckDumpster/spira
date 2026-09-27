#!/usr/bin/env bash
set -uo pipefail

is_done() {
    landed "$1" "$2"
}

mark_it() {
    land_mark "$1" LANDED "$2"
}

cite_it() {
    landed_sha "$1" "$2"
}
