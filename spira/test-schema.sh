#!/usr/bin/env bash
#
# test-schema.sh — schema.sh resolves every declared name and FAILS CLOSED on anything else.
#
#   ./test-schema.sh
#
# THE DEFECT THIS GUARDS. A name that resolves to the empty string is worse than one that
# errors: an empty label in a query is a well-formed question about nothing. It returns []
# truthfully, the refusal guard sees output rather than silence, and the pane prints a
# confident 0 — which is sp-xrkuu, where SP_READY read 0 against a true 32 and the first
# hypothesis was staleness, because a wrong answer and a correct one look identical from
# outside.
#
# So the load-bearing assertion here is the NEGATIVE one: an undeclared key must exit
# non-zero and print nothing usable on stdout. The positive cases are the control that proves
# the negative is not passing by accident (law-a-regression-test-must-be-seen-to-fail).
#
# tier: T1
# covers: spira/*.sh
# hermetic-ok: no database, no systemd, no network; conf.sh is pointed at a nonexistent file
# timeout: 60
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

eq()   { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }

S="$HERE/schema.sh"
echo "test-schema.sh"

echo
echo "every declared name resolves to something non-empty"
for k in ask ci scope spike groomer maechen maechen_remedy review reclaim_skip world_stop insight; do
    v="$("$S" name "$k" 2>/dev/null)"; rc=$?
    if [ "$rc" = 0 ] && [ -n "$v" ]; then ok "name $k -> $v"; else bad "name $k" "rc=$rc value=[$v]"; fi
done

echo
echo "NEGATIVE — an undeclared name fails closed (this is the assertion that matters)"
out="$("$S" name definitely-not-a-name 2>/dev/null)"; rc=$?
[ "$rc" != 0 ] && ok "undeclared name exits non-zero (rc=$rc)" || bad "undeclared name" "exited 0"
[ -z "$out" ]  && ok "undeclared name prints nothing on stdout" || bad "undeclared name stdout" "got [$out]"

out="$("$S" name "" 2>/dev/null)"; rc=$?
[ "$rc" != 0 ] && ok "empty name exits non-zero (rc=$rc)" || bad "empty name" "exited 0"

echo
echo "kinds and their types"
eq "kind work is a task"        "task"        "$("$S" type-of work)"
eq "kind event is an event"     "event"       "$("$S" type-of event)"
eq "kind insight is a chore"    "chore"       "$("$S" type-of insight)"
eq "kind escalation"            "escalation"  "$("$S" type-of escalation)"
out="$("$S" type-of not-a-kind 2>/dev/null)"; rc=$?
[ "$rc" != 0 ] && ok "undeclared kind fails closed (rc=$rc)" || bad "undeclared kind" "exited 0"

echo
echo "insight is NOT a custom type — it is a closed chore"
ct="$("$S" custom-types | tr '\n' ' ')"
want "custom types carry escalation" "escalation" "$ct"
want "custom types carry proposal"   "proposal"   "$ct"
want "custom types carry event"      "event"      "$ct"
want "custom types carry gate"       "gate"       "$ct"
[[ "$ct" != *"chore"* ]] && ok "custom types do NOT carry chore (it is built in)" \
                          || bad "custom types" "chore must not be registered: [$ct]"

echo
echo "dimensions"
want "dims include repo"  "repo"  "$("$S" dims | tr '\n' ' ')"
want "dims include gate"  "gate"  "$("$S" dims | tr '\n' ' ')"
out="$("$S" contract 2>/dev/null)"
want "contract names the store section" "STORE" "$out"
want "contract reads personas from the chamber" "PERSONAS" "$out"

echo
echo "the whole point: no OTHER harness file may carry a declared literal"
# Advisory here (the gate-wired lint is its own bead, sp-4wd); this asserts the property is
# measurable and reports the current count so the lint's starting point is known.
lits=0
for k in ask ci groomer maechen spike; do
    v="$("$S" name "$k")"
    c=$(grep -rlF -- "$v" "$HERE"/*.sh 2>/dev/null | grep -v -e '/schema.sh$' -e '/test-' | wc -l)
    lits=$((lits+c))
done
printf '  note — %s harness file(s) still carry a declared literal (sp-4wd wires the gate lint)\n' "$lits"

echo
tl_summary
