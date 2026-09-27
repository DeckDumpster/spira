#!/usr/bin/env bash
set -uo pipefail

bead_reopen() {
    bdq "$@"
}

always_reopen() {
    bdq reopen "$1"
}
