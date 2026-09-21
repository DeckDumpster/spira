#!/usr/bin/env bash
#
# test-install-loom-broker.sh — install skips spira-loom and spira-broker when
# their compiled binaries are absent.
#
# WHAT THIS SUITE PROVES
# ----------------------
# loom.sh and broker.sh are executable wrappers, so they pass install.sh's
# ExecStart check. The units would still cycle in activating forever because
# the wrappers exec the compiled binary, which does not exist on a box without
# cargo. The fix: units.sh omits both units when the binaries are absent. (sp-s5i6u)
#
# FOUR PROPERTIES are verified, all required:
#
#   A  POSITIVE CONTROL — loom binary present: UNITS includes spira-loom.service
#      and ENABLE includes the instance-qualified name.
#
#   B  SKIP — loom binary absent: spira-loom.service in OPTIONAL, absent from
#      UNITS and ENABLE; note printed to stderr.
#
#   C  POSITIVE CONTROL — broker binary present: UNITS includes spira-broker.service
#      and spira-broker.timer; ENABLE includes the instance-qualified timer name.
#
#   D  SKIP — broker binary absent: both broker units in OPTIONAL, absent from
#      UNITS and ENABLE; note printed to stderr.
#
# covers: systemd/units.sh systemd/install.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "${2:-}"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }

echo "test-install-loom-broker.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT
mkdir -p "$TMP/run"

# Stub binaries: executable files that satisfy [ -x ].
STUB_LOOM="$TMP/loom"
STUB_BROKER="$TMP/broker"
printf '#!/bin/sh\nexec "$@"\n' > "$STUB_LOOM" && chmod +x "$STUB_LOOM"
printf '#!/bin/sh\nexec "$@"\n' > "$STUB_BROKER" && chmod +x "$STUB_BROKER"

# query_units <loom-bin> <broker-bin> — source units.sh in a subprocess and
# print UNITS, OPTIONAL, and ENABLE arrays.  Pass "" to simulate absent binary.
query_units() {
    local loom_bin="$1" broker_bin="$2"
    bash - <<SUBSH
set -uo pipefail
SPIRA_HOME="$HERE"
SPIRA_INSTANCE=prod
SPIRA_DOLT_DATA=
SPIRA_TESTDB_DATA=
SPIRA_CONF=/nonexistent
SPIRA_RUN="$TMP/run"
SPIRA_LOOM_BIN="$loom_bin"
SPIRA_BROKER_BIN="$broker_bin"
export SPIRA_HOME SPIRA_INSTANCE SPIRA_DOLT_DATA SPIRA_TESTDB_DATA
export SPIRA_CONF SPIRA_RUN SPIRA_LOOM_BIN SPIRA_BROKER_BIN
. "$HERE/../systemd/units.sh" 2>&1 >&3
printf 'UNITS: %s\n' "\${UNITS[*]}"
printf 'OPTIONAL: %s\n' "\${OPTIONAL[*]}"
printf 'ENABLE: %s\n' "\${ENABLE[*]}"
SUBSH
}

# query_units redirects stderr notes to stdout so we can capture both together.
# Rewrite to capture them separately.
query_units_split() {
    local loom_bin="$1" broker_bin="$2"
    bash - 3>/dev/null <<SUBSH
set -uo pipefail
SPIRA_HOME="$HERE"
SPIRA_INSTANCE=prod
SPIRA_DOLT_DATA=
SPIRA_TESTDB_DATA=
SPIRA_CONF=/nonexistent
SPIRA_RUN="$TMP/run"
SPIRA_LOOM_BIN="$loom_bin"
SPIRA_BROKER_BIN="$broker_bin"
export SPIRA_HOME SPIRA_INSTANCE SPIRA_DOLT_DATA SPIRA_TESTDB_DATA
export SPIRA_CONF SPIRA_RUN SPIRA_LOOM_BIN SPIRA_BROKER_BIN
notes=\$( . "$HERE/../systemd/units.sh" 2>&1 >/dev/null )
. "$HERE/../systemd/units.sh" 2>/dev/null
printf 'UNITS: %s\n' "\${UNITS[*]}"
printf 'OPTIONAL: %s\n' "\${OPTIONAL[*]}"
printf 'ENABLE: %s\n' "\${ENABLE[*]}"
printf 'NOTES: %s\n' "\$notes"
SUBSH
}

# ==========================================================================
echo
echo "A: POSITIVE CONTROL — loom binary present → spira-loom in UNITS and ENABLE:"
# ==========================================================================
loom_pos="$(query_units_split "$STUB_LOOM" "$STUB_BROKER")"; rc=$?
is     "A: units.sh exits 0 with loom binary present"                  "0" "$rc"
want   "A: UNITS includes spira-loom.service"  "spira-loom.service" \
       "$(printf '%s\n' "$loom_pos" | grep '^UNITS:')"
want   "A: ENABLE includes spira-loom-prod.service"  "spira-loom-prod.service" \
       "$(printf '%s\n' "$loom_pos" | grep '^ENABLE:')"
nowant "A: OPTIONAL does not include spira-loom"  "spira-loom" \
       "$(printf '%s\n' "$loom_pos" | grep '^OPTIONAL:')"

# ==========================================================================
echo
echo "B: SKIP — loom binary absent → spira-loom in OPTIONAL, note printed:"
# ==========================================================================
loom_neg="$(query_units_split "" "$STUB_BROKER")"; rc=$?
is     "B: units.sh exits 0 with loom binary absent"                   "0" "$rc"
nowant "B: UNITS does not include spira-loom.service"  "spira-loom.service" \
       "$(printf '%s\n' "$loom_neg" | grep '^UNITS:')"
nowant "B: ENABLE does not include spira-loom-prod.service"  "spira-loom-prod.service" \
       "$(printf '%s\n' "$loom_neg" | grep '^ENABLE:')"
want   "B: OPTIONAL includes spira-loom.service"  "spira-loom.service" \
       "$(printf '%s\n' "$loom_neg" | grep '^OPTIONAL:')"
want   "B: note printed for absent loom binary"  "not installing spira-loom.service" \
       "$(printf '%s\n' "$loom_neg" | grep '^NOTES:')"

# ==========================================================================
echo
echo "C: POSITIVE CONTROL — broker binary present → spira-broker in UNITS and ENABLE:"
# ==========================================================================
want   "C: UNITS includes spira-broker.service"  "spira-broker.service" \
       "$(printf '%s\n' "$loom_pos" | grep '^UNITS:')"
want   "C: UNITS includes spira-broker.timer"  "spira-broker.timer" \
       "$(printf '%s\n' "$loom_pos" | grep '^UNITS:')"
want   "C: ENABLE includes spira-broker-prod.timer"  "spira-broker-prod.timer" \
       "$(printf '%s\n' "$loom_pos" | grep '^ENABLE:')"
nowant "C: OPTIONAL does not include spira-broker"  "spira-broker" \
       "$(printf '%s\n' "$loom_pos" | grep '^OPTIONAL:')"

# ==========================================================================
echo
echo "D: SKIP — broker binary absent → spira-broker in OPTIONAL, note printed:"
# ==========================================================================
broker_neg="$(query_units_split "$STUB_LOOM" "")"; rc=$?
is     "D: units.sh exits 0 with broker binary absent"                 "0" "$rc"
nowant "D: UNITS does not include spira-broker.service"  "spira-broker.service" \
       "$(printf '%s\n' "$broker_neg" | grep '^UNITS:')"
nowant "D: ENABLE does not include spira-broker-prod.timer"  "spira-broker-prod.timer" \
       "$(printf '%s\n' "$broker_neg" | grep '^ENABLE:')"
want   "D: OPTIONAL includes spira-broker.service"  "spira-broker.service" \
       "$(printf '%s\n' "$broker_neg" | grep '^OPTIONAL:')"
want   "D: note printed for absent broker binary"  "not installing spira-broker.service" \
       "$(printf '%s\n' "$broker_neg" | grep '^NOTES:')"

# ==========================================================================
echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" = 0 ]
