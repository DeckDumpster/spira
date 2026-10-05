#!/usr/bin/env bash
set -uo pipefail
# A reintroduced oracle, through the binary and through the ledger's files.
if env SPIRA_RUN="$RUN" landing-pass landed "$1" "$2"; then
    cut -d' ' -f1 < "$RUN/landstate/$1"
fi
