#!/usr/bin/env bash
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/lib.sh"

do_reopen() {
    bead_reopen reopen sp-1
}

do_reopen

do_always() {
    always_reopen sp-2
}
