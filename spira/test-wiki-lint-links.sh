#!/usr/bin/env bash
#
# test-wiki-lint-links.sh — positive control for wiki-lint-links.sh.
#
# THE DEFECT THIS GUARDS AGAINST. A commit adds a [[wikilink]] to index.md naming a page
# that commit does not track — the archivist's own commit rule requires staging each path
# explicitly by name, and a page named in the index but omitted from the git add line fails
# silently: the commit succeeds and reports success (incident: sp-8pjbh).
#
# THE POSITIVE CONTROL IS THE FIRST ASSERTION. An offender is planted (a link staged with
# no matching page) and the check is required to name it (SEEN RED) before its silence on a
# resolved link is trusted as evidence of a clean commit (law-absence-needs-a-positive-control).
#
# host-reason: tests wiki-lint-links.sh against scratch git repositories only
#
# tier: T0
# covers: spira/wiki-lint-links.sh spira/wiki-hooks/pre-commit spira/wiki-hooks-install.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

echo "test-wiki-lint-links.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

WIKI="$TMP/wiki"; mkdir -p "$WIKI"
git init -q -b main "$WIKI"
git -C "$WIKI" config user.email t@t; git -C "$WIKI" config user.name t
printf '# index\n' > "$WIKI/index.md"
git -C "$WIKI" add index.md
git -C "$WIKI" commit -q -m init

lint() { bash "$HERE/wiki-lint-links.sh" "$WIKI" 2>&1; }

# ---------------------------------------------------------------------------------------
# SEEN RED: a link added to index.md names a page this commit does not track.
# ---------------------------------------------------------------------------------------
printf '# index\n\n- [[a-page-nobody-wrote]] — dangling\n' > "$WIKI/index.md"
git -C "$WIKI" add index.md

out="$(lint)"; rc=$?
is   "SEEN RED: dangling link refused" "1" "$rc"
want "names the offending link"        "a-page-nobody-wrote" "$out"

git -C "$WIKI" restore --staged index.md
git -C "$WIKI" checkout -q -- index.md

# ---------------------------------------------------------------------------------------
# SEEN GREEN: the same link, but the page it names is staged in the same commit.
# ---------------------------------------------------------------------------------------
printf '# index\n\n- [[a-page-somebody-wrote]] — present\n' > "$WIKI/index.md"
printf '# a page\n' > "$WIKI/a-page-somebody-wrote.md"
git -C "$WIKI" add index.md a-page-somebody-wrote.md

out="$(lint)"; rc=$?
is "SEEN GREEN: resolved link passes" "0" "$rc"

git -C "$WIKI" commit -q -m "add a page and link it"

# ---------------------------------------------------------------------------------------
# A pre-existing dangling link, untouched by this commit, does not newly fail it.
# ---------------------------------------------------------------------------------------
printf '# index\n\n- [[a-page-somebody-wrote]] — present\n- [[already-broken]] — was already dangling\n' > "$WIKI/index.md"
git -C "$WIKI" add index.md
git -C "$WIKI" commit -q -m "introduce a pre-existing dangling link (out of band)"

printf '# index\n\n- [[a-page-somebody-wrote]] — present, note edited\n- [[already-broken]] — was already dangling\n' > "$WIKI/index.md"
git -C "$WIKI" add index.md

out="$(lint)"; rc=$?
is "unrelated edit to an already-broken index does not fail" "0" "$rc"

# ---------------------------------------------------------------------------------------
# HOOK WIRING: pre-commit actually refuses the commit, not just the standalone check.
# ---------------------------------------------------------------------------------------
git -C "$WIKI" config core.hooksPath "$HERE/wiki-hooks"
printf '# index\n\n- [[a-page-somebody-wrote]] — present\n- [[already-broken]] — was already dangling\n- [[yet-another-missing-page]] — new dangling link\n' > "$WIKI/index.md"
git -C "$WIKI" add index.md

out="$(git -C "$WIKI" commit -q -m "try to land a new dangling link" 2>&1)"; rc=$?
is   "hook refuses the commit"     "1" "$rc"
want "names the offending link"    "yet-another-missing-page" "$out"

git -C "$WIKI" restore --staged index.md
git -C "$WIKI" checkout -q -- index.md

tl_summary
