#!/usr/bin/env bash
#
# test-docs-links.sh — README.md and docs/ carry no broken relative link or anchor, no
# README anchor reference in the tree dangles, and the README stays a front door.
#
# tier: T1
# covers: README.md docs/** spira/docs-links.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

echo "test-docs-links.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
mkdir -p "$TMP/docs"
printf '# Top\n[ok](docs/a.md#sec) [gone](docs/missing.md) [bad](docs/a.md#nope)\n' > "$TMP/README.md"
printf '# A\n\n## Sec\n' > "$TMP/docs/a.md"
printf 'see README.md#nowhere\n' > "$TMP/note.txt"
git -C "$TMP" init -q && git -C "$TMP" add -A
out="$("$HERE/docs-links.sh" "$TMP" 2>&1)"; rc=$?
is     "SEEN RED: a planted broken link set fails" "1" "$rc"
want   "a missing file is named"        "docs/missing.md: no such file" "$out"
want   "a missing anchor is named"      "docs/a.md#nope: no such anchor" "$out"
want   "a dangling README anchor is named" "README.md#nowhere" "$out"
nowant "a good link is not reported"    "docs/a.md#sec" "$out"

out="$("$HERE/docs-links.sh" "$HERE/.." 2>&1)"; rc=$?
is     "the shipped README and docs have no broken links" "0" "$rc"
[ "$rc" = 0 ] || printf '%s\n' "$out"
tl_summary
