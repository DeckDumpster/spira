#!/usr/bin/env bash
#
# test-verify-asks.sh — asks with a passing VERIFY check are closed by verify-asks.sh.
#
#   ./test-verify-asks.sh
#
# WHAT THIS SUITE IS FOR
# ----------------------
# An operator ask can carry a read-only shell command that proves the work is already
# done. verify-asks.sh runs those commands unattended, and closes the ones that pass.
# Five properties are load-bearing and verified here:
#
#   1. A VERIFY check exiting 0 → verify-asks.sh closes the ask (with --apply).
#   2. A VERIFY check exiting non-zero → ask stays open.
#   3. An ask without VERIFY is never touched.
#   4. Without --apply the sweep reports but does not close.
#   5. A mislabelled epic (escalation label on an epic issue type) is reported.
#
# THE POSITIVE CONTROL IS FIRST. Before asserting that a passing check closes an ask,
# the suite verifies the bead is STILL OPEN before the sweep runs. A bead already
# closed would produce the same "closed" output and every assertion below would pass
# on fiction.
#
# SPIRA_ASK_LABEL IS PINNED TO A NON-DEFAULT to prevent a hardcoded literal in the
# parser from satisfying these assertions without exercising the key
# (law-gates-run-in-a-clean-environment). Both ask.sh (which writes the label) and
# verify-asks.sh (which reads it) source conf.sh, which honours the exported value.
#
# covers: cockpit/verify-asks.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
COCKPIT="$(cd "$HERE/../cockpit" && pwd -P)"

export SPIRA_ASK_LABEL=needs-attention
export SPIRA_CONF=/nonexistent   # use shipped defaults for everything else

. "$HERE/testdb.sh"
testdb_require test-verify-asks
testdb_up verifyasks || { echo "testdb_up failed"; exit 1; }

pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "expected [$2] got [$3]"; }

export COCKPIT_DB="$SPIRA_DB"
trap 'testdb_drop' EXIT INT TERM

ASK="${SPIRA_ASK_LABEL}"

ask()    { bash "$COCKPIT/ask.sh"         "$@"; }
verify() { bash "$COCKPIT/verify-asks.sh" "$@"; }

bd_show_status() {
    bd -C "$SPIRA_DB" show "$1" --json 2>/dev/null \
        | python3 -c 'import json,sys; r=json.load(sys.stdin); print(r[0].get("status","?") if isinstance(r,list) else r.get("status","?"))'
}

# Seed a bead that is an open ask.
seed1() {   # seed1 <id> <desc-json-string>
    printf '{"id":"%s","title":"t %s","description":"%s","status":"open","issue_type":"decision","labels":["%s","overseer","ask-question"]}\n' \
        "$1" "$1" "$2" "$ASK" | testdb_seed
}

echo "test-verify-asks.sh"

# ======================================================================================
echo
echo "a passing VERIFY check (exit 0) closes the ask:"
# ======================================================================================
# THE POSITIVE CONTROL: the bead must be open before the sweep runs, so a pre-closed
# bead cannot produce a false positive.
seed1 "sp-vok1" "subscribe the feed\\n\\nVERIFY: exit 0"

is "bead is open before sweep"  "open" "$(bd_show_status sp-vok1)"

out=$(verify --apply 2>&1)
want "SATISFIED is reported"     "SATISFIED"  "$out"
want "and names the bead"        "sp-vok1"    "$out"
want "and says closed"           "closed"     "$out"
is "bead is closed after sweep"  "closed" "$(bd_show_status sp-vok1)"

# The close reason names the command.
reason=$(bd -C "$SPIRA_DB" show sp-vok1 --json 2>/dev/null \
    | python3 -c 'import json,sys; r=json.load(sys.stdin); print(r[0].get("close_reason","") if isinstance(r,list) else r.get("close_reason",""))')
want "close reason cites the VERIFY command"   "VERIFY check now passes"  "$reason"
want "and quotes the command itself"           "exit 0"                   "$reason"

# ======================================================================================
echo
echo "a failing VERIFY check (exit non-zero) leaves the ask open:"
# ======================================================================================
testdb_reset

seed1 "sp-vfail1" "still open\\n\\nVERIFY: exit 1"

verify --apply >/dev/null 2>&1
is "a non-zero check leaves the ask open"  "open" "$(bd_show_status sp-vfail1)"

out=$(verify 2>&1)
want "non-zero check is reported as 'still open'"  "still open"  "$out"
want "and names the bead"                          "sp-vfail1"   "$out"

# ======================================================================================
echo
echo "asks without VERIFY are never touched:"
# ======================================================================================
testdb_reset

seed1 "sp-vnopred" "no check line here"

verify --apply >/dev/null 2>&1
is "an ask without VERIFY stays open"  "open" "$(bd_show_status sp-vnopred)"

out=$(verify 2>&1)
want "summary shows zero asks with a check"  "0 ask(s) carry a check"  "$out"

# ======================================================================================
echo
echo "without --apply the sweep reports but does not close:"
# ======================================================================================
testdb_reset

seed1 "sp-vdry1" "dry run test\\n\\nVERIFY: exit 0"

out=$(verify 2>&1)   # no --apply
want  "SATISFIED is reported"               "SATISFIED"  "$out"
want  "and names the bead"                  "sp-vdry1"   "$out"
nowant "but the close confirmation is absent" "    closed" "$out"
is "bead stays open without --apply"  "open" "$(bd_show_status sp-vdry1)"

# ======================================================================================
echo
echo "mislabelled epics (epics with the ask label) are reported:"
# ======================================================================================
testdb_reset

printf '{"id":"sp-vepic1","title":"my epic","description":"VERIFY: exit 0","status":"open","issue_type":"epic","labels":["%s","overseer"]}\n' \
    "$ASK" | testdb_seed

out=$(verify 2>&1)
want "mislabelled epic is reported"       "MISLABELLED"  "$out"
want "and names the bead"                 "sp-vepic1"    "$out"
want "and names the issue type"           "epic"         "$out"

# A mislabelled epic must not be closed — it is an error, not a satisfied check.
is "mislabelled epic stays open"  "open" "$(bd_show_status sp-vepic1)"

# ======================================================================================
echo
echo "already-closed asks are not re-closed:"
# ======================================================================================
testdb_reset

printf '{"id":"sp-valready","title":"t","description":"VERIFY: exit 0","status":"closed","issue_type":"decision","labels":["%s","overseer","ask-question"]}\n' \
    "$ASK" | testdb_seed

out=$(verify 2>&1)
nowant "a closed ask is not re-reported"   "sp-valready"   "$out"
want   "and counts as 0 checks found"     "0 ask(s) carry a check"  "$out"

# ======================================================================================
echo
printf '%s: %d passed, %d failed\n' "$(basename "$0")" "$pass" "$fail"
[ "$fail" -eq 0 ]
