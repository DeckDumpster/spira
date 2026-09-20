#!/usr/bin/env bash
# covers: spira/hooks/aeon-fence.sh spira/testdb.sh
# defect: sp-mvg44
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
refuse() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "must not contain [$2] in [$3]"; }

echo "test-aeon-fence-bd-create.sh"

. "$HERE/testdb.sh"
testdb_require test-aeon-fence-bd-create.sh
testdb_up bd-create-fence
TMP="$(mktemp -d)"
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM

FENCE="$HERE/hooks/aeon-fence.sh"

# Minimal repo-map: spira and brain are valid; fixture-repo is not.
FAKE_REPO_MAP="$TMP/repo-map"
printf 'spira | /dev/null | push\nbrain | /dev/null | push\n' > "$FAKE_REPO_MAP"

# fence_bd <cmd> [VAR=val ...]: pipe a Bash-tool payload to the fence.
# The fence's environment has SPIRA_DB pointing at the testdb (the "production" store
# in this fixture) and SPIRA_REPO_MAP pointing at the minimal map above.
fence_bd() {
    local cmd="$1"; shift
    local payload
    payload="$(printf '%s' "$cmd" | python3 -c 'import json, sys; cmd = sys.stdin.read(); print(json.dumps({"tool_name":"Bash","tool_input":{"command":cmd}}))')"
    printf '%s' "$payload" | env -i PATH="$PATH" HOME="$TMP" \
        SPIRA_AEON=test-aeon \
        SPIRA_DB="$SPIRA_DB" \
        SPIRA_BD="$SPIRA_BD" \
        SPIRA_REPO_MAP="$FAKE_REPO_MAP" \
        "$@" \
        bash "$FENCE" 2>/dev/null
}

PROD="$SPIRA_DB"   # testdb acts as "production" for this fixture

# ===========================================================================
echo
echo "POSITIVE CONTROL — fence detects test-data title before asserting the block tests:"
# ===========================================================================
pc="$(fence_bd "bd -C $PROD create \"test-land-state\" -l \"plan\"")"
want "POSITIVE: fence blocks test-data title" '"decision":"block"' "$pc"

# ===========================================================================
echo
echo "A — the three commands from sp-wrosb blocked:"
# ===========================================================================

out="$(fence_bd "bd -C $PROD create \"test-land-state\" -l \"plan\"")"
want "A1: test-land-state title → decision:block"   '"decision":"block"' "$out"
want "A1: reason mentions override var"              "SPIRA_BD_CREATE_OVERRIDE" "$out"

out="$(fence_bd "bd -C $PROD create \"test-land-state\" -l \"plan\"")"
want "A2: same command re-run → decision:block"     '"decision":"block"' "$out"

# Command 3 has an inline SPIRA_DB pointing at the testdb but -C at production.
# The fence reads -C and compares it to its environment SPIRA_DB (the production path),
# so the inline override does not let the create through.
out="$(fence_bd "SPIRA_DB=/tmp/some-testdb bd -C $PROD create \"test bead\" -l \"plan,repo:fixture-repo\"")"
want "A3: test-bead + unmapped repo → decision:block" '"decision":"block"' "$out"
want "A3: reason mentions override var"               "SPIRA_BD_CREATE_OVERRIDE" "$out"

# ===========================================================================
echo
echo "B — control creates that should pass:"
# ===========================================================================

# Legitimate title, mapped repo.
out="$(fence_bd "bd -C $PROD create \"Implement config reload\" -l \"plan,repo:spira\"")"
refuse "B1: real title + mapped repo NOT blocked"    '"decision":"block"' "$out"

# Targeting a different (test) db path entirely — not production.
out="$(fence_bd "bd -C /tmp/other-testdb create \"test bead\" -l \"plan\"")"
refuse "B2: bd create to non-SPIRA_DB path NOT blocked" '"decision":"block"' "$out"

# Plain bd create with SPIRA_DB inline-overridden to a temp path.
out="$(fence_bd "SPIRA_DB=/tmp/local-testdb bd create \"test fixture bead\"")"
refuse "B3: inline SPIRA_DB override to non-prod NOT blocked" '"decision":"block"' "$out"

# ===========================================================================
echo
echo "C — SPIRA_BD_CREATE_OVERRIDE=1 bypasses the fence:"
# ===========================================================================
out="$(fence_bd "SPIRA_BD_CREATE_OVERRIDE=1 bd -C $PROD create \"test bead\" -l \"plan,repo:fixture-repo\"")"
refuse "C1: SPIRA_BD_CREATE_OVERRIDE=1 bypasses block" '"decision":"block"' "$out"

# ===========================================================================
echo
echo "D — unmapped repo: label alone (with non-test title) is blocked:"
# ===========================================================================
out="$(fence_bd "bd -C $PROD create \"Add lane detection\" -l \"plan,repo:fixture-repo\"")"
want "D1: unmapped repo with real title → decision:block" '"decision":"block"' "$out"
want "D1: reason mentions override var"                   "SPIRA_BD_CREATE_OVERRIDE" "$out"

# ===========================================================================
echo
printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" = 0 ]
