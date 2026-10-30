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
# covers: spira/*.sh UC-config-store-preflight-19
# hermetic-ok: no database, no systemd, no network; conf.sh is pointed at a nonexistent file
# timeout: 90
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

eq()   { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }

echo "test-schema.sh"

echo
echo "every declared name resolves to something non-empty"
for k in ask ci scope spike groomer mimir mimir_remedy review world_stop insight; do
    v="$(schema.sh name "$k" 2>/dev/null)"; rc=$?
    if [ "$rc" = 0 ] && [ -n "$v" ]; then ok "name $k -> $v"; else bad "name $k" "rc=$rc value=[$v]"; fi
done

echo
echo "NEGATIVE — an undeclared name fails closed (this is the assertion that matters)"
out="$(schema.sh name definitely-not-a-name 2>/dev/null)"; rc=$?
[ "$rc" != 0 ] && ok "undeclared name exits non-zero (rc=$rc)" || bad "undeclared name" "exited 0"
[ -z "$out" ]  && ok "undeclared name prints nothing on stdout" || bad "undeclared name stdout" "got [$out]"

out="$(schema.sh name "" 2>/dev/null)"; rc=$?
[ "$rc" != 0 ] && ok "empty name exits non-zero (rc=$rc)" || bad "empty name" "exited 0"

echo
echo "kinds and their types"
eq "kind work is a task"        "task"        "$(schema.sh type-of work)"
eq "kind event is an event"     "event"       "$(schema.sh type-of event)"
eq "kind insight is a chore"    "chore"       "$(schema.sh type-of insight)"
eq "kind escalation"            "escalation"  "$(schema.sh type-of escalation)"
eq "kind idea is a proposal"    "proposal"    "$(schema.sh type-of idea)"
eq "idea label name"            "idea"        "$(schema.sh name idea)"
out="$(schema.sh type-of not-a-kind 2>/dev/null)"; rc=$?
[ "$rc" != 0 ] && ok "undeclared kind fails closed (rc=$rc)" || bad "undeclared kind" "exited 0"

echo
echo "insight is NOT a custom type — it is a closed chore"
ct="$(schema.sh custom-types | tr '\n' ' ')"
want "custom types carry escalation" "escalation" "$ct"
want "custom types carry proposal"   "proposal"   "$ct"
want "custom types carry event"      "event"      "$ct"
want "custom types carry gate"       "gate"       "$ct"
[[ "$ct" != *"chore"* ]] && ok "custom types do NOT carry chore (it is built in)" \
                          || bad "custom types" "chore must not be registered: [$ct]"

echo
echo "dimensions"
want "dims include repo"  "repo"  "$(schema.sh dims | tr '\n' ' ')"
want "dims include gate"  "gate"  "$(schema.sh dims | tr '\n' ' ')"
out="$(schema.sh contract 2>/dev/null)"
want "contract names the store section" "STORE" "$out"
want "contract reads personas from the runes" "PERSONAS" "$out"

echo
echo "the whole point: no OTHER harness file may carry a declared literal"
# Advisory here (the gate-wired lint is its own bead, sp-4wd); this asserts the property is
# measurable and reports the current count so the lint's starting point is known.
lits=0
for k in ask ci groomer mimir spike; do
    v="$(schema.sh name "$k")"
    c=$(grep -rlF -- "$v" "$HERE"/*.sh 2>/dev/null | grep -v -e '/schema.sh$' -e '/test-' | wc -l)
    lits=$((lits+c))
done
printf '  note — %s harness file(s) still carry a declared literal (sp-4wd wires the gate lint)\n' "$lits"

echo
echo "a slow or failing store is cannot-tell, never MISSING"
shimdir="$(mktemp -d)"; trap 'rm -rf "$shimdir"' EXIT
mkdir -p "$shimdir/db"
types="$(schema.sh custom-types | paste -sd, -)"; sts="$(schema.sh statuses | paste -sd, -)"
cat >"$shimdir/bd" <<SHIM
#!/usr/bin/env bash
: >"$shimdir/shim-ran"
args="\$*"
case "\$args" in
    *"config get types.custom"*) [ -n "\${SHIM_SLOW_TYPES:-}" ] && sleep "\$SHIM_SLOW_TYPES"; echo "$types" ;;
    *"config get status.custom"*) echo "$sts" ;;
    *" sql "*) [ -n "\${SHIM_SQL_ERR:-}" ] && { echo "\$SHIM_SQL_ERR" >&2; exit 1; }
               printf '+-------+\n| count |\n%s\n' "\${SHIM_COUNT:-1}" ;;
esac
SHIM
chmod +x "$shimdir/bd"
sc() { PATH="$shimdir:$PATH" SPIRA_DB="$shimdir/db" "$@" 2>&1; }
dl="$(schema.sh probe-deadline)"

out="$(sc bash "$HERE/schema.sh" check)"; rc=$?
[ -e "$shimdir/shim-ran" ] && ok "the shim is the bd schema.sh resolves" || bad "shim resolution" "shim never ran"
is "healthy store exits 0" 0 "$rc"
want "healthy store matches" "store matches the declaration" "$out"

out="$(SHIM_SLOW_TYPES=$((dl+1)) sc bash "$HERE/schema.sh" check)"; rc=$?
is "slow probe exits 3" 3 "$rc"
want "slow probe names the timeout" "cannot tell: types.custom timed out after ${dl}s" "$out"
nowant "slow probe claims no MISSING" "MISSING custom type" "$out"

out="$(SHIM_SQL_ERR='Error: connection refused' sc bash "$HERE/schema.sh" check)"; rc=$?
is "erroring probe exits 3" 3 "$rc"
want "erroring probe carries bd's error" "cannot tell: _is_work probe: Error: connection refused" "$out"
nowant "erroring probe claims no MISSING" "MISSING generated column" "$out"

out="$(SHIM_COUNT=0 sc bash "$HERE/schema.sh" check)"; rc=$?
is "honest absence exits 1" 1 "$rc"
want "honest absence is MISSING" "MISSING generated column: _is_work" "$out"

out="$(SHIM_COUNT=oops sc bash "$HERE/schema.sh" check)"; rc=$?
is "non-integer count exits 3" 3 "$rc"
want "non-integer count is cannot-tell" "cannot tell: _is_work probe returned no count: " "$out"
nowant "non-integer count claims no MISSING" "MISSING generated column" "$out"

out="$(SHIM_SLOW_TYPES=$((dl+1)) SHIM_COUNT=0 sc bash "$HERE/schema.sh" check)"; rc=$?
is "drift plus cannot-tell exits 1" 1 "$rc"
want "drift is printed" "MISSING generated column: _is_work" "$out"
want "cannot-tell is printed" "cannot tell: types.custom timed out" "$out"

out="$(SHIM_SLOW_TYPES=$((dl+1)) sc bash "$HERE/schema.sh" contract)"
want "contract STORE shows cannot-tell" "custom types:    schema: cannot tell: types.custom timed out" "$out"

echo
echo "schema-apply.sh ends in a passing check on a store that is already correct"
out="$(sc bash "$HERE/schema-apply.sh")"; rc=$?
is "apply exits 0 when its closing check passes" 0 "$rc"
want "apply reaches its closing check" "applied. verifying with schema.sh check" "$out"
want "and the check's own verdict is in the output" "store matches the declaration" "$out"
[ "$rc" = 0 ] || printf '%s\n' "$out" | sed 's/^/    /'

echo
echo "schema-apply.sh reports applied-not-verified for a cannot-tell"
out="$(SHIM_SLOW_TYPES=$((dl+1)) sc bash "$HERE/schema-apply.sh")"; rc=$?
is "apply exits 4" 4 "$rc"
want "apply says applied, NOT verified" "applied, NOT verified" "$out"

echo
tl_summary
