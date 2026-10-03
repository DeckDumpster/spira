#!/usr/bin/env bash
# tier: T2
# covers: .gitattributes spira-config/schema/spira-key-history.txt
#
# Two branches that each append a config key to the append-only key history must merge
# without a conflict; the repository's .gitattributes is what makes that so.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"
echo "test-key-history-merge.sh"

H=spira-config/schema/spira-key-history.txt
t="$(mktemp -d)"
g() { git -C "$t" -c user.email=t@t -c user.name=t -c merge.conflictstyle=merge "$@"; }
git init -q -b main "$t"
mkdir -p "$t/spira-config/schema"
printf 'alpha\nbeta\n' > "$t/$H"
g add -A; g commit -q -m base
for k in one two; do
  g checkout -q -b "$k" main
  echo "key_$k" >> "$t/$H"
  g commit -q -am "add $k"
done

g checkout -q one
g merge -q --no-edit two >/dev/null 2>&1; rc=$?
is "without the attribute, concurrent appends conflict (positive control)" 1 "$rc"
g merge --abort >/dev/null 2>&1

cp "$HERE/../.gitattributes" "$t/.gitattributes"
g add .gitattributes
g commit -q -m attrs
g merge -q --no-edit two2 >/dev/null 2>&1; rc=$?
is "with the repository's .gitattributes, they merge cleanly" 0 "$rc"
merged="$(g show HEAD:$H 2>/dev/null)"
want "the merge keeps one's line" key_one "$merged"
want "the merge keeps two's line" key_two "$merged"
rm -rf "$t"

tl_summary
