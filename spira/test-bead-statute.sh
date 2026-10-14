#!/usr/bin/env bash
#
# test-bead-statute.sh — bead.sh file/amend run the statute check on their own text: a
# contradicting decision is refused naming the statute, the named override admits it, and
# every match is recorded on the bead.
#
#   ./test-bead-statute.sh
#
# POSITIVE CONTROL FIRST (law-absence-needs-a-positive-control): the refusal is seen to fire
# before the silent cases are believed.
#
# tier: T1
# covers: spira/bead.sh spira/statute-guards spira/conf.d/SPIRA_STATUTE_GUARDS bead/src/statute.rs bead/src/main.rs
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"
. "$HERE/testdb.sh"

echo "test-bead-statute.sh"

TMP="$(mktemp -d)"
MAP="$TMP/repos.tbl"
printf 'starsite | /srv/starsite | push | origin/main |  |\n' > "$MAP"
REG="$TMP/guards"
cat > "$REG" <<'ROWS'
law-the-allocator-assigns-the-identifier | refuse | pick \w+ from a configured range
law-fixture-touch | show | allocator
ROWS

testdb_require test-bead-statute
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up bead_statute || { printf 'test-bead-statute: could not build fixture database\n' >&2; exit 1; }
testdb_seed <<'JSONL'
{"id":"sp-stt-a","title":"statute target","description":"old","status":"open","issue_type":"task","labels":["repo:starsite","plan"],"updated_at":"2026-09-25T00:00:00Z"}
JSONL

BD="${SPIRA_BD:-$TESTDB_BD}"
show() { timeout 5 "$BD" -C "$SPIRA_DB" show "$1" --json 2>/dev/null; }
find_id() { timeout 5 "$BD" -C "$SPIRA_DB" list --json --all --limit 0 2>/dev/null \
    | python3 -c 'import json,sys; t=sys.argv[1]; d=[x for x in json.load(sys.stdin) if x["title"]==t]; print(d[0]["id"] if d else "")' "$1"; }

run() {   # run <registry> args... ; BEAD_STATUTE_CONSIDERED passes through when set
    local reg="$1"; shift
    tl_config SPIRA_REPO_MAP="$MAP" SPIRA_BD="$BD" SPIRA_DB="$SPIRA_DB" SPIRA_RUN="$TMP/run" SPIRA_STATUTE_GUARDS="$reg"
    env -i SPIRA_TOML="$SPIRA_TOML" PATH="$PATH" HOME="$TMP" \
        SPIRA_HOME="$HERE" SPIRA_REPO="$TMP/norepo" BD_TIMEOUT=10 \
        ${BEAD_STATUTE_CONSIDERED:+BEAD_STATUTE_CONSIDERED="$BEAD_STATUTE_CONSIDERED"} \
        bead.sh "$@" > "$TMP/out" 2>&1
}

printf 'Pick VMIDs from a configured range.\n' > "$TMP/bad.md"
printf 'Ask the allocator for the next VMID.\n' > "$TMP/touch.md"
printf 'tidy the readme\n' > "$TMP/clean.md"
unset BEAD_STATUTE_CONSIDERED

echo; echo "POSITIVE CONTROL: a contradicting body is refused, the statute named, nothing filed:"
run "$REG" file "VMID-BAD" --kind event --repo starsite --body-file "$TMP/bad.md"; rc=$?; out="$(cat "$TMP/out")"
is     "contradiction: exit 2"          2 "$rc"
want   "contradiction: names statute"   "law-the-allocator-assigns-the-identifier" "$out"
want   "contradiction: names the exit"  "BEAD_STATUTE_CONSIDERED" "$out"
is     "contradiction: bead not created" "" "$(find_id VMID-BAD)"

echo; echo "a persona-filed work bead gets the same treatment:"
run "$REG" file "VMID-WORK" --for builder --repo starsite --body-file "$TMP/bad.md"; rc=$?
is "persona: exit 2 [$(tr '\n' ' ' < "$TMP/out" | head -c 200)]" 2 "$rc"
is "persona: bead not created" "" "$(find_id VMID-WORK)"

echo; echo "a contradiction in the title alone is refused:"
run "$REG" file "pick VMIDs from a configured range" --kind event --repo starsite; rc=$?
is "title: exit 2" 2 "$rc"

echo; echo "clean text and an inert registry are admitted:"
run "$REG" file "CLEAN" --kind event --repo starsite --body-file "$TMP/clean.md"; rc=$?
is   "clean: exit 0" 0 "$rc"
nowant "clean: no statute line" "Statute check" "$(show "$(find_id CLEAN)")"
run "" file "INERT" --kind event --repo starsite --body-file "$TMP/bad.md"; rc=$?
is "empty key: exit 0" 0 "$rc"

echo; echo "a registry that cannot be read refuses rather than passing:"
run "$TMP/nonexistent" file "NOREG" --kind event --repo starsite --body-file "$TMP/clean.md"; rc=$?
is "unreadable registry: exit 1" 1 "$rc"
printf 'law-x | maybe | y\n' > "$TMP/badreg"
run "$TMP/badreg" file "BADREG" --kind event --repo starsite --body-file "$TMP/clean.md"; rc=$?
is "malformed registry: exit 1" 1 "$rc"

echo; echo "a touching (show) match is admitted and recorded in the bead:"
run "$REG" file "TOUCH" --kind event --repo starsite --body-file "$TMP/touch.md"; rc=$?
is   "touch: exit 0" 0 "$rc"
want "touch: shown to the actor" "law-fixture-touch" "$(cat "$TMP/out")"
want "touch: recorded on the bead" "Statute check: law-fixture-touch" "$(show "$(find_id TOUCH)")"

echo; echo "the override admits a contradiction and records slug and reason:"
BEAD_STATUTE_CONSIDERED="fixture range is not a VMID" \
    run "$REG" file "OVERRIDE" --kind event --repo starsite --body-file "$TMP/bad.md"; rc=$?
is   "override: exit 0" 0 "$rc"
D="$(show "$(find_id OVERRIDE)")"
want "override: slug recorded"   "law-the-allocator-assigns-the-identifier" "$D"
want "override: reason recorded" "override: fixture range is not a VMID" "$D"
BEAD_STATUTE_CONSIDERED="  " run "$REG" file "BLANK" --kind event --repo starsite --body-file "$TMP/bad.md"; rc=$?
is "blank override: still refused" 2 "$rc"

echo; echo "amend runs the same check:"
run "$REG" amend sp-stt-a --note "pick VMIDs from a configured range"; rc=$?
is     "amend contradiction: exit 2" 2 "$rc"
nowant "amend contradiction: nothing written" "configured range" "$(show sp-stt-a)"
run "$REG" amend sp-stt-a --note "ask the allocator"; rc=$?
is   "amend touch: exit 0" 0 "$rc"
want "amend touch: statute noted" "Statute check: law-fixture-touch" "$(show sp-stt-a)"
BEAD_STATUTE_CONSIDERED="why" run "$REG" amend sp-stt-a --note "pick VMIDs from a configured range"; rc=$?
is   "amend override: exit 0" 0 "$rc"
want "amend override: reason noted" "override: why" "$(show sp-stt-a)"

echo
tl_summary
