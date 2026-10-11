#!/usr/bin/env bash
#
# test-bead-amend.sh — bead.sh amend --body-file refuses without --replace; --append-body
# keeps the old description and adds the new text.
#
# tier: T2
# covers: spira/bead.sh bead/src/main.rs bead/src/lib.rs
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
. "$HERE/testdb.sh"

echo "test-bead-amend.sh"

testdb_require test-bead-amend
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up bead_amend || { printf 'test-bead-amend: could not build fixture database\n' >&2; exit 1; }

testdb_seed <<'JSONL'
{"id":"sp-amd-a","title":"amend target a","description":"ORIGINAL-A line one\nline two","status":"open","issue_type":"task","labels":["repo:spira","plan"],"updated_at":"2026-09-25T00:00:00Z"}
{"id":"sp-amd-b","title":"amend target b","description":"ORIGINAL-B","status":"open","issue_type":"task","labels":["repo:spira","plan"],"updated_at":"2026-09-25T00:00:00Z"}
JSONL

run_amend() {
    tl_config SPIRA_DB="$SPIRA_DB" SPIRA_BD="${SPIRA_BD:-$TESTDB_BD}" SPIRA_RUN="$TMP/run"
    AM_OUT="$(SPIRA_HOME="$HERE" SPIRA_CONF="$TMP/no.conf" bead.sh amend "$@" 2>&1)"
    AM_RC=$?
}

desc_of() {
    timeout 5 "${SPIRA_BD:-$TESTDB_BD}" -C "$SPIRA_DB" show "$1" --json 2>/dev/null \
        | python3 -c 'import json,sys; d=json.load(sys.stdin); d=d[0] if isinstance(d,list) else d; print(d.get("description",""))'
}

printf 'NEW-BODY\n' > "$TMP/new.md"

echo
echo "T1: --body-file alone refuses, naming --replace and --append-body"
run_amend sp-amd-a --body-file "$TMP/new.md"
wantrc "refused" "2" "$AM_RC"
want "names --replace" "--replace" "$AM_OUT"
want "names --append-body" "--append-body" "$AM_OUT"
want "old body intact" "ORIGINAL-A" "$(desc_of sp-amd-a)"
nowant "new body not written" "NEW-BODY" "$(desc_of sp-amd-a)"

echo
echo "T2: --replace replaces and shows what was lost"
run_amend sp-amd-a --body-file "$TMP/new.md" --replace
wantrc "replace ok" "0" "$AM_RC"
want "prints the old body's head" "ORIGINAL-A line one" "$AM_OUT"
want "prints the old length" "bytes" "$AM_OUT"
want "new body written" "NEW-BODY" "$(desc_of sp-amd-a)"
nowant "old body gone" "ORIGINAL-A" "$(desc_of sp-amd-a)"

echo
echo "T3: --append-body keeps the old text and adds the new"
run_amend sp-amd-b --append-body "$TMP/new.md"
wantrc "append ok" "0" "$AM_RC"
want "old text kept" "ORIGINAL-B" "$(desc_of sp-amd-b)"
want "new text added" "NEW-BODY" "$(desc_of sp-amd-b)"
want "dated separator" "--- appended 20" "$(desc_of sp-amd-b)"

tl_summary
