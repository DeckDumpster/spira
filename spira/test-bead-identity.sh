#!/usr/bin/env bash
#
# test-bead-identity.sh — bead.sh file/amend refuse a bead for one repo that carries another
# registered repo's identity terms; the named override admits it and records the reason.
#
#   ./test-bead-identity.sh
#
# POSITIVE CONTROL FIRST (law-absence-needs-a-positive-control): the refusal is seen to fire
# before the silent cases (kit, own terms, no registry) are believed.
#
# tier: T1
# covers: spira/bead.sh spira/conf.d/SPIRA_REPO_IDENTITY bead/src/identity.rs bead/src/main.rs
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

echo "test-bead-identity.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

MAP="$TMP/repos.tbl"
{
    printf 'starsite | /srv/starsite | push | origin/main |  |\n'
    printf 'dexsite  | /srv/dexsite  | push | origin/main |  |\n'
    printf 'dumpster-kit | /srv/kit  | push | origin/main |  |\n'
} > "$MAP"
REG="$TMP/identity"
printf 'dexsite | Pokédex, Pokémon\nstarsite | starsite\n' > "$REG"

# A stub bd: create copies its --body-file; show answers with a starsite bead; note and
# update record their argv.
STUB_BD="$TMP/bd"
cat > "$STUB_BD" <<STUB
#!/usr/bin/env bash
case "\$1" in
    show) printf '[{"id":"x-1","labels":["repo:starsite"],"status":"open","issue_type":"task","priority":2,"description":"old"}]\n' ;;
    create) while [ \$# -gt 0 ]; do [ "\$1" = --body-file ] && cp "\$2" "$TMP/body"; shift; done; echo bd-called ;;
    note) printf '%s\n' "\$*" >> "$TMP/notes"; echo bd-called ;;
    *) echo bd-called ;;
esac
exit 0
STUB
chmod +x "$STUB_BD"

run() {   # run <identity-file> -- args...  (BEAD_CROSS_REPO_CONSIDERED passes through when set)
    local id="$1"; shift
    rm -f "$TMP/body" "$TMP/notes"
    tl_config SPIRA_REPO_MAP="$MAP" SPIRA_BD="$STUB_BD" SPIRA_REPO_IDENTITY="$id"
    env -i SPIRA_TOML="$SPIRA_TOML" PATH="$PATH" HOME="$TMP" \
        SPIRA_HOME="$HERE" SPIRA_REPO="$TMP/norepo" SPIRA_DB="$TMP/nodb" BD_TIMEOUT=10 \
        ${BEAD_CROSS_REPO_CONSIDERED:+BEAD_CROSS_REPO_CONSIDERED="$BEAD_CROSS_REPO_CONSIDERED"} \
        bead.sh "$@" > "$TMP/out" 2>&1
}

printf 'extend the banned-names test with Pokédex terms\n' > "$TMP/foreign.md"
printf 'route this through dumpster-kit\n' > "$TMP/kit.md"
printf 'fix the starsite header\n' > "$TMP/own.md"

echo; echo "POSITIVE CONTROL: another repo's term is refused, naming the owner, bd not called:"
unset BEAD_CROSS_REPO_CONSIDERED
run "$REG" file "banned names" --kind event --repo starsite --body-file "$TMP/foreign.md"; rc=$?; out="$(cat "$TMP/out")"
is     "foreign term: exit 2"           2 "$rc"
want   "foreign term: names the term"   "pokédex" "$out"
want   "foreign term: names the owner"  "dexsite" "$out"
want   "foreign term: names the override" "BEAD_CROSS_REPO_CONSIDERED" "$out"
nowant "foreign term: bd not called"    "bd-called" "$out"

echo; echo "a term in the title alone is refused:"
run "$REG" file "Pokémon cards" --kind event --repo starsite; rc=$?
is "title term: exit 2" 2 "$rc"

echo; echo "kit, own terms and an unset registry are admitted:"
run "$REG" file "kit work" --kind event --repo starsite --body-file "$TMP/kit.md"; rc=$?
is "kit named: exit 0" 0 "$rc"
run "$REG" file "own" --kind event --repo starsite --body-file "$TMP/own.md"; rc=$?
is "own term: exit 0" 0 "$rc"
run "" file "banned names" --kind event --repo starsite --body-file "$TMP/foreign.md"; rc=$?
is "no registry: exit 0" 0 "$rc"

echo; echo "the override admits the bead and records the reason on it:"
BEAD_CROSS_REPO_CONSIDERED="shared fixture, routed via the kit" \
    run "$REG" file "banned names" --kind event --repo starsite --body-file "$TMP/foreign.md"; rc=$?
is   "override: exit 0" 0 "$rc"
want "override: reason is in the body bd received" "Cross-repo considered: shared fixture, routed via the kit" "$(cat "$TMP/body" 2>/dev/null)"
want "override: original body kept" "Pokédex" "$(cat "$TMP/body" 2>/dev/null)"
BEAD_CROSS_REPO_CONSIDERED="   " run "$REG" file "x" --kind event --repo starsite --body-file "$TMP/foreign.md"; rc=$?
is "blank override: still refused" 2 "$rc"

echo; echo "amend is guarded by the bead's own repo label:"
unset BEAD_CROSS_REPO_CONSIDERED
run "$REG" amend x-1 --note "also cover Pokédex"; rc=$?; out="$(cat "$TMP/out")"
is     "amend foreign: exit 2" 2 "$rc"
want   "amend foreign: names owner" "dexsite" "$out"
nowant "amend foreign: no note written" "Pokédex" "$(cat "$TMP/notes" 2>/dev/null)"
run "$REG" amend x-1 --note "clean note"; rc=$?
is "amend clean: exit 0" 0 "$rc"
BEAD_CROSS_REPO_CONSIDERED="spans repos" run "$REG" amend x-1 --note "also cover Pokédex"; rc=$?
is   "amend override: exit 0" 0 "$rc"
want "amend override: reason noted" "Cross-repo considered: spans repos" "$(cat "$TMP/notes" 2>/dev/null)"

echo
tl_summary
