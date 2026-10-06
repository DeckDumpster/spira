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
# tier: T1
# covers: install/src/manifest.rs spira/conf.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

echo "test-broker-units.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

# get_enable BROKER_ENABLE
# units-install --list-enable (sp-31dm0: systemd/units.sh is retired; the manifest is
# install/src/manifest.rs now), one entry per line, in a minimal env.
get_enable() {
    local broker_enable="$1"
    tl_config SPIRA_INSTANCE=prod SPIRA_DOLT_DATA="" SPIRA_TESTDB_DATA="" \
        SPIRA_SELF_TEST=0 SPIRA_BROKER_ENABLE="$broker_enable"
    env -i \
        PATH="$PATH" \
        HOME="$HOME" \
        SPIRA_TOML="$SPIRA_TOML" \
        SPIRA_HOME="$HERE" \
        SPIRA_REPO="$(cd "$HERE/.." && pwd -P)" \
        units-install --list-enable 2>/dev/null
}

# ==========================================================================
echo
echo "A: POSITIVE CONTROL — SPIRA_BROKER_ENABLE=1: broker timer in ENABLE:"
# ==========================================================================
pos_out="$(get_enable 1)"
want "A: broker-prod.timer in ENABLE when BROKER_ENABLE=1" \
    "spira-broker-prod.timer" "$pos_out"

# ==========================================================================
echo
echo "B: NO PRODUCER — SPIRA_BROKER_ENABLE=0: broker timer absent from ENABLE:"
# ==========================================================================
noprod_out="$(get_enable 0)"
nowant "B: broker-prod.timer absent from ENABLE when BROKER_ENABLE=0" \
    "spira-broker-prod.timer" "$noprod_out"

# (case C — "binary missing" — is deleted with the not-built state, sp-gypjk: a release always
# carries broker.)

tl_summary
