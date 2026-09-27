#!/usr/bin/env bash
# test-install-collisions.sh — systemd/install.sh's path-collision refusal, as a table
# over all five collision keys.
#
# _check_path_collisions scans *.conf files beside SPIRA_CONF_FILE and refuses when
# another instance's file sets the same value for SPIRA_RUN, SPIRA_DB, SPIRA_PROD,
# SPIRA_DOLT_DATA or SPIRA_TESTDB_PORT. Sourcing install.sh with SPIRA_INSTALL_LIB=1
# stops the file right after the function is defined: no conf.sh, no systemd, no
# rendered DEST tree. Replaces the old container-gated test-install-paths.sh (T2,
# real systemd, 39s) — gap G7, docs/test-plan/instance-lifecycle.md: only SPIRA_RUN
# was covered there, and its "no unit written after refusal" check compared `ls
# "$DEST" | wc -l` against itself under the same variable name, so it always passed.
# This T1 suite never touches DEST at all, which is the fix: there is nothing there
# to write.
#
# tier: T1
# covers: systemd/install.sh UC-instance-lifecycle-20
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
iszero()  { [ "$2" = 0 ] && ok "$1" || bad "$1" "wanted exit 0, got $2"; }
nonzero() { [ "$2" != 0 ] && ok "$1" || bad "$1" "wanted non-zero exit, got 0"; }

echo "test-install-collisions.sh"

SPIRA_INSTALL_LIB=1 . "$HERE/../systemd/install.sh"
if ! declare -F _check_path_collisions >/dev/null; then
    bail "SPIRA_INSTALL_LIB=1 did not define _check_path_collisions — install.sh's guard is broken"
fi

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

# run <this-instance> <key> <this-value> <other-conf-line...> -> stdout+rc of
# _check_path_collisions in a subshell, with SPIRA_CONF_FILE pointed at a file that
# need not exist ($TMP/this.conf) and the other five keys unset — an unset key must
# be excluded from comparison, not compared as an empty string.
run() {
    local this_inst="$1" key="$2" val="$3"; shift 3
    (
        unset SPIRA_RUN SPIRA_DB SPIRA_PROD SPIRA_DOLT_DATA SPIRA_TESTDB_PORT
        export SPIRA_INSTANCE="$this_inst"
        export "$key=$val"
        printf '%s\n' "$@" > "$TMP/other.conf"
        SPIRA_CONF_FILE="$TMP/this.conf" _check_path_collisions 2>&1
        echo "RC=$?"
    )
}

KEYS=(SPIRA_RUN SPIRA_DB SPIRA_PROD SPIRA_DOLT_DATA SPIRA_TESTDB_PORT)

# ==========================================================================
echo
echo "POSITIVE CONTROL — a collision on each of the five keys is detected:"
# ==========================================================================
for key in "${KEYS[@]}"; do
    out="$(run test "$key" "shared-value" "SPIRA_INSTANCE = prod" "$key = shared-value")"
    rc="${out##*RC=}"
    nonzero "$key: exit non-zero when it collides"     "$rc"
    want    "$key: names the colliding key"            "$key"          "$out"
    want    "$key: names the other instance"           "prod"          "$out"
    want    "$key: names the other config file"        "other.conf"    "$out"
    want    "$key: names the shared value"             "shared-value"  "$out"
done

# ==========================================================================
echo
echo "DISTINCT VALUES — a different value for the same key does not block install:"
# ==========================================================================
for key in "${KEYS[@]}"; do
    out="$(run test "$key" "this-value" "SPIRA_INSTANCE = prod" "$key = other-value")"
    rc="${out##*RC=}"
    iszero "$key: exit 0 when values differ"    "$rc"
    nowant "$key: no refusal message"           "refusing" "$out"
done

# ==========================================================================
echo
echo "SAME INSTANCE — a matching value is not a collision when the instance matches:"
# ==========================================================================
out="$(run test SPIRA_RUN "shared-value" "SPIRA_INSTANCE = test" "SPIRA_RUN = shared-value")"
rc="${out##*RC=}"
iszero "same-instance: exit 0 when other config has the same SPIRA_INSTANCE" "$rc"
nowant "same-instance: no refusal message"                                    "refusing" "$out"

# ==========================================================================
echo
echo "NO CONFIG FILE — an unset SPIRA_CONF_FILE skips the check entirely:"
# ==========================================================================
no_conf_out="$(
    unset SPIRA_RUN SPIRA_DB SPIRA_PROD SPIRA_DOLT_DATA SPIRA_TESTDB_PORT SPIRA_CONF_FILE
    export SPIRA_INSTANCE=prod SPIRA_RUN=shared-value
    printf 'SPIRA_INSTANCE = other\nSPIRA_RUN = shared-value\n' > "$TMP/other.conf"
    _check_path_collisions
    echo "RC=$?"
)"
no_conf_rc="${no_conf_out##*RC=}"
iszero "no-conf: exit 0 when SPIRA_CONF_FILE is unset (check skipped)" "$no_conf_rc"

tl_summary
