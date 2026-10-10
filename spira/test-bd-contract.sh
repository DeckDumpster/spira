#!/usr/bin/env bash
#
# test-bd-contract.sh — what the harness assumes of bd, asserted against a real bd on a real
# initialised store: the store carries an issue_prefix, ids are minted from it, list/show
# --json have the shapes the readers parse, and a tripped circuit breaker refuses until its
# file is cleared (so clearing must come before any probe).
#
# tier: T2
# covers: spira/lib.sh spira/conf.sh install/src/bin/install.rs UC-config-store-preflight-25
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
. "$HERE/testdb.sh"
testdb_require test-bd-contract
TMP="$(mktemp -d)"
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up bdcontract || bail "could not build fixture database"

echo "test-bd-contract.sh"

echo
echo "real bd init: prefix and id minting:"
prefix="$(bd -C "$SPIRA_DB" config get issue_prefix 2>/dev/null | tr -d '[:space:]')" # batch-job: fixture bd call against the suite's throwaway store
[ -n "$prefix" ] && ok "initialised store reports an issue_prefix ($prefix)" \
    || bad "initialised store reports an issue_prefix" "config get issue_prefix printed nothing"
id="$(bd -C "$SPIRA_DB" create "contract probe" -l plan --silent 2>/dev/null | tr -d '[:space:]')" # batch-job: fixture bd call against the suite's throwaway store
case "$id" in
    "$prefix"-*) ok "create --silent prints a bare id starting with the prefix ($id)" ;;
    *) bad "create --silent prints a bare id starting with the prefix" "prefix [$prefix] id [$id]" ;;
esac

echo
echo "json shapes the readers parse:"
shown="$(bd -C "$SPIRA_DB" show "$id" --json 2>/dev/null)" # batch-job: fixture bd call against the suite's throwaway store
is "show --json carries the id" "$id" "$(printf '%s' "$shown" | python3 -c 'import sys,json; d=json.load(sys.stdin); d=d[0] if isinstance(d,list) else d; print(d.get("id",""))' 2>/dev/null)"
listed="$(bd -C "$SPIRA_DB" list --json -l plan 2>/dev/null | python3 -c 'import sys,json; print(",".join(x["id"] for x in json.load(sys.stdin)))' 2>/dev/null)" # batch-job: fixture bd call against the suite's throwaway store
want "list --json -l <label> returns the labelled bead" "$id" "$listed"
other="$(bd -C "$SPIRA_DB" list --json -l no-such-label 2>/dev/null | python3 -c 'import sys,json; print(len(json.load(sys.stdin)))' 2>/dev/null)" # batch-job: fixture bd call against the suite's throwaway store
is "list --json -l <absent label> is an empty array" "0" "$other"

echo
echo "circuit breaker ordering:"
reserve_port port
name="bdcontract$$"
mkdir -p "$TMP/srv/.beads"
printf '{"dolt_mode":"server","dolt_server_port":%s,"dolt_database":"%s","project_id":"test"}\n' \
    "$port" "$name" > "$TMP/srv/.beads/metadata.json"
for _ in 1 2 3 4 5 6 7 8 9 10; do
    BD_NON_INTERACTIVE=1 timeout 5 bd -C "$TMP/srv" list --json >/dev/null 2>&1 & # batch-job: trips the breaker against a closed port
done
wait
flag="/tmp/beads-circuit/beads-dolt-circuit-127-0-0-1-${port}-${name}.json"
if [ -f "$flag" ]; then
    ok "positive control: repeated failures write a breaker file"
    tripped="$(BD_NON_INTERACTIVE=1 timeout 5 bd -C "$TMP/srv" list --json 2>&1)" # batch-job: probe with breaker open
    want "open breaker refuses before dialling" "circuit breaker" "$tripped"
    rm -f "$flag"
    cleared="$(BD_NON_INTERACTIVE=1 timeout 5 bd -C "$TMP/srv" list --json 2>&1)" # batch-job: probe after clearing the breaker
    nowant "clearing the breaker file lifts the refusal" "circuit breaker is open" "$cleared"
else
    bad "positive control: repeated failures write a breaker file" "expected $flag; bd's breaker path changed"
fi
rm -f "$flag"

echo
tl_summary
