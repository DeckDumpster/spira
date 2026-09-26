#!/usr/bin/env bash
#
# test-install-conf-seed.sh — systemd/install.sh's _seed_instance_conf, called directly.
# systemd/install.sh defines it before sourcing conf.sh and returns without rendering or
# installing anything when sourced (BASH_SOURCE[0] != $0), so this suite never renders a
# unit, never touches systemctl, and never mocks a prod checkout.
#
#   ./test-install-conf-seed.sh
#
# PROPERTIES UNDER TEST
# ---------------------
# 1. SEED: writes SPIRA_INSTANCE=<instance> to an absent-or-unseeded file and reports it.
# 2. IDEMPOTENT: a second call on an already-seeded file appends no duplicate line and
#    reports nothing (the caller in systemd/install.sh treats silence as "unchanged").
# 3. EXISTING FILE PRESERVED: a file with prior operator settings keeps them; the seed
#    line is appended, not written over them.
#
# POSITIVE CONTROL: the seed must be verified to have fired (property 1) before property 2
# claims idempotency — a call that never writes anything would look idempotent too.
#
# The instance id is allocated per run ($$) rather than a fixed "test" literal, so this
# suite cannot collide with another instance of itself or of a sibling suite sharing a
# fixed name (G14).
#
# tier: T1
# covers: systemd/install.sh UC-instance-lifecycle-29
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"

filehas() {    # filehas <name> <line> <file>
    grep -qxF "$2" "$3" 2>/dev/null && ok "$1" || bad "$1" "wanted line [$2] in $3"
}
filecount() {  # filecount <name> <line> <file> <n>
    local n; n="$(grep -cxF "$2" "$3" 2>/dev/null || echo 0)"
    [ "$n" = "$4" ] && ok "$1" || bad "$1" "wanted $4 occurrences of [$2], got $n in $3"
}

echo "test-install-conf-seed.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

# Sourcing systemd/install.sh (not executing it) defines _seed_instance_conf and returns
# before conf.sh, unit rendering or the systemctl apply loop run.
. "$HERE/../systemd/install.sh"

INSTANCE="test$$"
CONF="$TMP/spira.conf"

# ===========================================================================
echo
echo "POSITIVE CONTROL: seeding an absent file writes the line and reports it"
# ===========================================================================
out="$(_seed_instance_conf "$CONF" "$INSTANCE")"
filehas "seed: SPIRA_INSTANCE=$INSTANCE written to $CONF" "SPIRA_INSTANCE=$INSTANCE" "$CONF"
want    "seed: reports the seeding"                       "seeded" "$out"

# ===========================================================================
echo
echo "IDEMPOTENT: a second call appends no duplicate line and is silent"
# ===========================================================================
out2="$(_seed_instance_conf "$CONF" "$INSTANCE")"
filecount "idempotent: SPIRA_INSTANCE=$INSTANCE appears exactly once" \
          "SPIRA_INSTANCE=$INSTANCE" "$CONF" "1"
is "idempotent: second call reports nothing" "" "$out2"

# ===========================================================================
echo
echo "EXISTING FILE PRESERVED: prior operator settings survive the seed"
# ===========================================================================
rm -f "$CONF"
printf 'SPIRA_DB=/some/path/db\n' > "$CONF"

_seed_instance_conf "$CONF" "$INSTANCE" >/dev/null
filehas "existing: original SPIRA_DB line preserved"  "SPIRA_DB=/some/path/db"    "$CONF"
filehas "existing: SPIRA_INSTANCE=$INSTANCE appended" "SPIRA_INSTANCE=$INSTANCE" "$CONF"

tl_summary
