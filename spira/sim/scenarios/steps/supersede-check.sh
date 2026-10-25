#!/bin/sh
# supersede-check.sh — what the duty left on the bd side and in the Concierge's inbox. Exit 3 until the
# request that needs a landed successor is SUPERSEDED, 9 on the first claim that does not hold.
set -u
[ "$(spira-lc state sp-su02 2>/dev/null)" = SUPERSEDED ] || exit 3
fail() { echo "supersede-check: $1" >&2; exit 9; }
note() { timeout 5 bd -C "$SIM_BEADS_DB" show "$1" 2>/dev/null | grep -qF "$2" || fail "$1 has no note: $2"; }
note sp-su02 "supersede-request confirmed: sp-su01 is LANDED"
note sp-su03 "supersede-request by sp-su04 refused: sp-su04 is READY"
note sp-su05 "supersede-request names the bead itself"
note sp-su06 "supersede-request by sp-absent refused"
timeout 5 bd -C "$SIM_BEADS_DB" show sp-su02 --json 2>/dev/null | grep -q '"status": *"closed"' || fail "sp-su02 is not closed on the bd side"
inbox="$SPIRA_RUN/duty-inbox.log"
grep -qF "[watch:supersede-duty] SUPERSEDE REQUEST sp-su02" "$inbox" || fail "the Concierge was not told of sp-su02"
if grep -qF "SUPERSEDE REQUEST sp-su07" "$inbox"; then fail "the manual hold was adjudicated"; fi
