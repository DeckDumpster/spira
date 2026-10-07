#!/usr/bin/env bash
#
# bd-pin.sh — record and verify the installed bd binary's known migration count.
#
# WHY IT EXISTS. A rebuild that installs bd at a different migration count from the
# database cursor causes bd to exit 0 with its complaint on stdout — callers that check
# exit status read success and parse the error as data. Pinning the known migration count
# after every install makes an unannounced rebuild visible to doctor before the loop
# tries to run (see the SPIRA_BD_PIN key in conf.sh and the "bd schema" section in
# doctor).
#
#   bd-pin.sh show   — print the recorded pin (default)
#   bd-pin.sh write  — record current bd state to the pin file
#
# The pin file location is SPIRA_BD_PIN (from conf.sh; default $SPIRA_RUN/bd-pin).
# Run bd-pin.sh write right after building and installing a new bd binary.
#
# BUILD FLAGS TO RECORD. The bd binary on this box was built CGO_ENABLED=0 (statically
# linked, zero ICU symbols), which means it cannot use embedded Dolt and must talk to a
# dolt sql-server in server mode. The next person to rebuild needs to match this: a CGO
# build opens an embedded engine that does not talk to the existing server, and the two
# can diverge silently. The pin file records both so neither fact has to be re-derived.
#
# covers: spira/conf.sh doctor/src/*
set -uo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)/conf.sh"

PIN_FILE="${SPIRA_BD_PIN:-}"

cmd="${1:-show}"
case "$cmd" in
    write)
        # `bd migrate schema` exits 0 on match and prints "✓ Schema already at vNN".
        # NN is the highest migration this binary knows, which is what we pin.
        # batch-job: schema migration over the whole store
        if ! _out="$(timeout 30 bd -C "$SPIRA_DB" migrate schema 2>&1)"; then
            printf 'bd-pin: bd migrate schema failed — cannot determine migration count\n' >&2
            printf '%s\n' "$_out" | head -3 >&2
            exit 1
        fi
        _count="$(printf '%s\n' "$_out" | grep -oE 'already at v[0-9]+' | grep -oE '[0-9]+')"
        if [ -z "${_count:-}" ]; then
            printf 'bd-pin: unexpected output from bd migrate schema — no version number found\n' >&2
            printf '%s\n' "$_out" | head -3 >&2
            exit 1
        fi
        _ver="$(timeout 5 bd version 2>/dev/null | head -1 || printf 'unknown')"
        _bd_bin="$(command -v bd 2>/dev/null || true)"
        _sha="$(sha256sum "$_bd_bin" 2>/dev/null | awk '{print $1}' || printf 'unknown')"
        mkdir -p "$(dirname "$PIN_FILE")"
        # BD_PIN_BUILD and BD_PIN_MODE are recorded as facts about this build:
        # CGO_ENABLED=0 means embedded Dolt is unavailable; server mode is required.
        printf 'BD_PIN_VERSION=%s\nBD_PIN_MIGRATIONS=%s\nBD_PIN_SHA256=%s\nBD_PIN_BUILD=CGO_ENABLED=0\nBD_PIN_MODE=server\n' \
            "$_ver" "$_count" "$_sha" > "$PIN_FILE"
        printf 'recorded: v%s at %s\n' "$_count" "$PIN_FILE"
        ;;
    show|*)
        if [ -f "$PIN_FILE" ]; then
            cat "$PIN_FILE"
        else
            printf 'no pin file at %s\nrun: %s write\n' "$PIN_FILE" "$(basename "$0")"
        fi
        ;;
esac
