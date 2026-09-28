#!/usr/bin/env bash
#
# test-broker-units.sh — units.sh broker timer conditional enable.
#
# The broker timer leaves the unconditional ENABLE list: it should only appear
# in ENABLE when SPIRA_BROKER_ENABLE=1 AND the broker binary is executable.
#
# THREE PROPERTIES are verified, all three required for the fix to hold:
#
#   A  POSITIVE CONTROL: SPIRA_BROKER_ENABLE=1 with an executable binary → the
#      broker timer IS in the ENABLE set. Without this, the silence in B and C
#      could mean the timer was never reachable (law-absence-needs-a-positive-control).
#
#   B  NO PRODUCER: SPIRA_BROKER_ENABLE=0 (the default) → broker timer absent from
#      ENABLE regardless of binary state.
#
#   C  BINARY MISSING: SPIRA_BROKER_ENABLE=1 but binary not executable → broker
#      timer absent from ENABLE even though the opt-in is set.
#
# covers: systemd/units.sh spira/conf.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

echo "test-broker-units.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
FAKE_BROKER="$TMP/fake-broker"
printf '#!/bin/sh\n' > "$FAKE_BROKER"; chmod +x "$FAKE_BROKER"

# get_enable BROKER_ENABLE BROKER_BIN
# Source units.sh in a minimal env and print the ENABLE array, one entry per line.
get_enable() {
    local broker_enable="$1" broker_bin="$2"
    env -i \
        PATH="$PATH" \
        SPIRA_INSTANCE=prod \
        SPIRA_HOME="$HERE" \
        SPIRA_DOLT_DATA="" \
        SPIRA_TESTDB_DATA="" \
        SPIRA_BROKER_ENABLE="$broker_enable" \
        SPIRA_BROKER_BIN="$broker_bin" \
        SPIRA_SELF_TEST=0 \
        bash -c '
            . "$SPIRA_HOME/../systemd/units.sh" 2>/dev/null
            printf "%s\n" "${ENABLE[@]}"
        '
}

# ==========================================================================
echo
echo "A: POSITIVE CONTROL — SPIRA_BROKER_ENABLE=1 + binary present: broker timer in ENABLE:"
# ==========================================================================
pos_out="$(get_enable 1 "$FAKE_BROKER")"
want "A: broker-prod.timer in ENABLE when BROKER_ENABLE=1 and binary executable" \
    "spira-broker-prod.timer" "$pos_out"

# ==========================================================================
echo
echo "B: NO PRODUCER — SPIRA_BROKER_ENABLE=0: broker timer absent from ENABLE:"
# ==========================================================================
noprod_out="$(get_enable 0 "$FAKE_BROKER")"
nowant "B: broker-prod.timer absent from ENABLE when BROKER_ENABLE=0" \
    "spira-broker-prod.timer" "$noprod_out"

# ==========================================================================
echo
echo "C: BINARY MISSING — SPIRA_BROKER_ENABLE=1 but binary not executable: broker timer absent:"
# ==========================================================================
nomis_out="$(get_enable 1 "/nonexistent/broker")"
nowant "C: broker-prod.timer absent from ENABLE when binary not executable" \
    "spira-broker-prod.timer" "$nomis_out"

tl_summary
