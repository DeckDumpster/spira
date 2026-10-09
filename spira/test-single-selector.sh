#!/usr/bin/env bash
# test-single-selector.sh — suite selection has one implementation: every consumer links the
# suite-select crate and no private selector script is left in the tree.
# tier: T0
# covers: suite-select/Cargo.toml testenv/Cargo.toml batcher-cut/Cargo.toml gate/Cargo.toml .github/workflows/gate.yml UC-test-infrastructure-07
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"
ROOT="$(cd "$HERE/.." && pwd -P)"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM

links_selector() { grep -Eq '^suite-select[[:space:]]*=.*path[[:space:]]*=[[:space:]]*"\.\./suite-select"' "$1"; }

printf '[dependencies]\nserde = "1"\n' > "$TMP/plain.toml"
printf '[dependencies]\nsuite-select = { path = "../suite-select" }\n' > "$TMP/linked.toml"
links_selector "$TMP/linked.toml" && ok "matcher: a crate that links suite-select is recognised" || bad "matcher: linked fixture"
links_selector "$TMP/plain.toml"  && bad "matcher: a crate without it is recognised" "matched" || ok "matcher: a crate without suite-select is refused"

for c in testenv batcher-cut gate; do
    links_selector "$ROOT/$c/Cargo.toml" && ok "$c links the suite-select crate" || bad "$c links the suite-select crate" "no path dependency in $c/Cargo.toml"
done

grep -q 'cargo build --release -p suite-select' "$ROOT/.github/workflows/gate.yml" \
    && ok "gate.yml builds and selects with suite-select" || bad "gate.yml builds and selects with suite-select" "build step missing"

for s in select.sh gate-touched.sh gate-budget-select.sh gate-spira.sh suite-select.sh; do
    [ ! -e "$ROOT/spira/$s" ] && ok "no private selector script spira/$s" || bad "no private selector script spira/$s" "present"
done

tl_summary
