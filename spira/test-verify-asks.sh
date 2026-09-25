#!/usr/bin/env bash
# test-verify-asks.sh — asks with a passing VERIFY check are closed by verify-asks.sh.
#
# WHAT THIS SUITE IS FOR
# -----------------------
# An operator ask can carry a read-only shell command that proves the work is already done.
# verify-asks.sh runs those commands unattended and closes the ones that pass. The sweep
# CLASSIFICATION logic (docs/test-plan/operator-channel.md row 26: DEMOTE-TO-T1) runs against
# a stub `bd` and a fake COCKPIT_DB — it never reads what a real bd said, only what
# cockpit_beads handed it, so a fixture row is exactly as faithful as a real one. ONE case at
# the bottom runs the real close, at T3, so the stub's shape is checked against the truth it
# stands in for.
#
# THE POSITIVE CONTROL IS FIRST in every section: before trusting a "still open" or "not
# reported" result, the same fixture with a passing check must close
# (law-absence-needs-a-positive-control).
#
# G-04 (verify-asks safety): the timeout kills a hanging VERIFY (SPIRA_VERIFY_TIMEOUT, a
# conf.sh seam added for this suite — the shipped 120s default cannot be waited out here); a
# VERIFY with a side effect runs it, because nothing in the contract makes VERIFY read-only,
# only the convention documented at the top of verify-asks.sh; and the unreachable-DB branch
# prints "checked nothing" and exits 0. Whether that exit is fail-open BY DESIGN is genuinely
# ambiguous — escalated to the operator rather than picked here (see this bead's close
# reason) — so this suite pins the CURRENT behaviour without endorsing it as correct.
#
# tier: T1
# covers: cockpit/verify-asks.sh cockpit/db.sh spira/conf.sh UC-operator-channel-26
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
COCKPIT="$(cd "$HERE/../cockpit" && pwd -P)"
. "$HERE/testlib.sh"

export SPIRA_ASK_LABEL=needs-attention   # non-default (law-gates-run-in-a-clean-environment)
export SPIRA_CONF=/nonexistent

echo "test-verify-asks.sh"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM

ASK="${SPIRA_ASK_LABEL}"
verify() { bash "$COCKPIT/verify-asks.sh" "$@"; }

# ==========================================================================
# T1 — sweep classification against a stub bd (no testdb, no real bead store).
# ==========================================================================
FAKE_DB="$TMP/fakedb"; mkdir -p "$FAKE_DB/.beads"
export COCKPIT_DB="$FAKE_DB"
ROWS_FILE="$TMP/rows.json"
CLOSED_LOG="$TMP/closed.log"
export SPIRA_TESTBD_ROWS="$ROWS_FILE" SPIRA_TESTBD_CLOSED="$CLOSED_LOG"

STUB_BD="$TMP/bd"
cat > "$STUB_BD" <<'STUBEOF'
#!/usr/bin/env bash
# A stub standing in for bd against a fixture, not a fixture MODELLING bd: it answers exactly
# the two calls verify-asks.sh makes (list, close) and nothing else.
args=("$@")
[ "${args[0]:-}" = "-C" ] && args=("${args[@]:2}")
case "${args[0]:-}" in
    list)  cat "$SPIRA_TESTBD_ROWS" ;;
    close) printf '%s\n' "${args[*]}" >> "$SPIRA_TESTBD_CLOSED"; exit 0 ;;
    *)     exit 0 ;;
esac
STUBEOF
chmod +x "$STUB_BD"
export BD_BIN="$STUB_BD"

row() {   # row <id> <status> <issue_type> <extra-labels-csv-or-""> <description>
    local id="$1" status="$2" itype="$3" extra="$4" desc="$5" labels="\"$ASK\",\"overseer\""
    [ -n "$extra" ] && labels="$labels,\"$extra\""
    printf '{"id":"%s","title":"t %s","description":"%s","status":"%s","issue_type":"%s","labels":[%s]}' \
        "$id" "$id" "$desc" "$status" "$itype" "$labels"
}

echo
echo "a passing VERIFY check (exit 0) closes the ask:"

printf '[%s]' "$(row sp-vok1 open decision "" "subscribe the feed\\n\\nVERIFY: exit 0")" > "$ROWS_FILE"
: > "$CLOSED_LOG"
out="$(verify --apply 2>&1)"
want "SATISFIED is reported"    "SATISFIED"  "$out"
want "and names the bead"       "sp-vok1"    "$out"
want "and says closed"          "closed"     "$out"
close_call="$(cat "$CLOSED_LOG")"
want "the stub's close was actually called" "sp-vok1" "$close_call"
want "close reason cites the VERIFY command" "VERIFY check now passes" "$close_call"
want "and quotes the command itself"         "exit 0"                 "$close_call"

echo
echo "a failing VERIFY check (exit non-zero) leaves the ask open:"

printf '[%s]' "$(row sp-vfail1 open decision "" "still open\\n\\nVERIFY: exit 1")" > "$ROWS_FILE"
: > "$CLOSED_LOG"
out="$(verify --apply 2>&1)"
want "non-zero check is reported as 'still open'" "still open"  "$out"
want "and names the bead"                         "sp-vfail1"   "$out"
nowant "SEEN RED control: the stub's close was NOT called" "sp-vfail1" "$(cat "$CLOSED_LOG")"

echo
echo "asks without VERIFY are never touched:"

printf '[%s]' "$(row sp-vnopred open decision "" "no check line here")" > "$ROWS_FILE"
: > "$CLOSED_LOG"
out="$(verify --apply 2>&1)"
want "summary shows zero asks with a check" "0 ask(s) carry a check" "$out"
is "the stub's close was never called" "" "$(cat "$CLOSED_LOG")"

echo
echo "without --apply the sweep reports but does not close:"

printf '[%s]' "$(row sp-vdry1 open decision "" "dry run test\\n\\nVERIFY: exit 0")" > "$ROWS_FILE"
: > "$CLOSED_LOG"
out="$(verify 2>&1)"   # no --apply
want   "SATISFIED is reported"                "SATISFIED"  "$out"
want   "and names the bead"                   "sp-vdry1"   "$out"
nowant "but the close confirmation is absent" "    closed" "$out"
is "the stub's close was never called without --apply" "" "$(cat "$CLOSED_LOG")"

echo
echo "mislabelled epics (epics with the ask label) are reported:"

printf '[%s]' "$(row sp-vepic1 open epic "" "VERIFY: exit 0")" > "$ROWS_FILE"
: > "$CLOSED_LOG"
# No --apply: the MISLABELLED report and the close sweep are two independent passes over the
# same rows (verify-asks.sh's close loop does not itself exclude issue_type=epic — an epic
# that also carries a VERIFY line is not proven safe from --apply by this case; only that the
# report fires and that --apply's own gate, exercised elsewhere in this suite, still holds).
out="$(verify 2>&1)"
want "mislabelled epic is reported" "MISLABELLED" "$out"
want "and names the bead"           "sp-vepic1"   "$out"
want "and names the issue type"     "epic"        "$out"
is "nothing closes without --apply, epic included" "" "$(cat "$CLOSED_LOG")"

echo
echo "already-closed asks are not re-closed or re-reported:"

printf '[%s]' "$(row sp-valready closed decision "" "VERIFY: exit 0")" > "$ROWS_FILE"
: > "$CLOSED_LOG"
out="$(verify 2>&1)"
nowant "a closed ask is not re-reported"  "sp-valready"             "$out"
want   "and counts as 0 checks found"     "0 ask(s) carry a check"  "$out"

# ==========================================================================
# G-03/G-04 — suit-adjacent and verify-safety gaps, still against the stub.
# ==========================================================================
echo
echo "G-04: a hanging VERIFY is killed by the timeout, not waited out"

printf '[%s]' "$(row sp-vhang open decision "" "hangs\\n\\nVERIFY: sleep 5")" > "$ROWS_FILE"
: > "$CLOSED_LOG"
out="$(SPIRA_VERIFY_TIMEOUT=1 verify --apply 2>&1)"
want "a hanging VERIFY is reported as still open, not satisfied" "still open" "$out"
nowant "SEEN RED control: a hung check is never SATISFIED" "SATISFIED" "$out"
is "the stub's close was never called for a killed VERIFY" "" "$(cat "$CLOSED_LOG")"

echo
echo "G-04: VERIFY runs unsandboxed — a side effect it declares happens for real"

SIDE_FILE="$TMP/side-effect-happened"
rm -f "$SIDE_FILE"
printf '[%s]' "$(row sp-vside open decision "" "$(printf 'touches a file\\n\\nVERIFY: touch %s' "$SIDE_FILE")")" > "$ROWS_FILE"
verify --apply >/dev/null 2>&1
[ -f "$SIDE_FILE" ] && ok "a VERIFY with a side effect actually runs it (nothing sandboxes VERIFY)" \
    || bad "a VERIFY with a side effect actually runs it" "side-effect file was not created"

echo
echo "G-04: an unreachable database prints 'checked nothing' and exits 0 — pinned as the"
echo "      CURRENT behaviour; whether fail-open here is intended is escalated, not decided"

NO_BEADS_DIR="$TMP/no-beads-dir"; mkdir -p "$NO_BEADS_DIR"
out="$(COCKPIT_DB="$NO_BEADS_DIR" verify --apply 2>&1)"; rc=$?
want "a missing .beads dir prints 'checked nothing'" "checked nothing" "$out"
is "verify-asks.sh exits 0 even though it read nothing (missing .beads)" "0" "$rc"

# THE OTHER HALF: .beads exists (cockpit_db succeeds) but the engine itself refuses to answer
# — cockpit_beads() is what returns 1 here, a different branch of the same fail-open exit.
DEAD_BD="$TMP/dead-bd"
printf '#!/usr/bin/env bash\nexit 1\n' > "$DEAD_BD"; chmod +x "$DEAD_BD"
out="$(BD_BIN="$DEAD_BD" verify --apply 2>&1)"; rc=$?
want "an unreachable engine also prints 'checked nothing'" "checked nothing" "$out"
is "verify-asks.sh exits 0 even though the engine refused (unreachable)" "0" "$rc"

# ==========================================================================
# T3 — one real close, so the stub above is checked against the real bd it
# stands in for.
# ==========================================================================
echo
echo "one real close: a passing VERIFY closes a bead in a real bd"

unset BD_BIN COCKPIT_DB
# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-verify-asks
testdb_up verifyasks || { echo "testdb_up failed"; exit 1; }
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
export COCKPIT_DB="$SPIRA_DB"

bd_show_status() {
    bd -C "$SPIRA_DB" show "$1" --json 2>/dev/null \
        | python3 -c 'import json,sys; r=json.load(sys.stdin); print(r[0].get("status","?") if isinstance(r,list) else r.get("status","?"))'
}

printf '{"id":"sp-vreal1","title":"t sp-vreal1","description":"subscribe the feed\\n\\nVERIFY: exit 0","status":"open","issue_type":"decision","labels":["%s","overseer","ask-question"]}\n' \
    "$ASK" | testdb_seed
is "SEEN RED: bead is open before sweep" "open" "$(bd_show_status sp-vreal1)"
out="$(verify --apply 2>&1)"
want "SATISFIED is reported against a real bd"   "SATISFIED" "$out"
is   "bead is closed after the real sweep" "closed" "$(bd_show_status sp-vreal1)"

tl_summary
