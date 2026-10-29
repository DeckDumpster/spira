#!/usr/bin/env bash
# tier: T2
# covers: .gitattributes spira/conf.key-history spira/config-delta.toml
#
# Two branches that each append a config key to the append-only key history and to the config
# delta must merge without a conflict; the repository's .gitattributes is what makes that so.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"
echo "test-key-history-merge.sh"

H=spira/conf.key-history
t="$(mktemp -d)"
g() { git -C "$t" -c user.email=t@t -c user.name=t -c merge.conflictstyle=merge "$@"; }
git init -q -b main "$t"
mkdir -p "$t/spira"
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
g merge -q --no-edit two >/dev/null 2>&1; rc=$?
is "with the repository's .gitattributes, they merge cleanly" 0 "$rc"
merged="$(g show HEAD:$H 2>/dev/null)"
want "the merge keeps one's line" key_one "$merged"
want "the merge keeps two's line" key_two "$merged"
rm -rf "$t"

D=spira/config-delta.toml
t2="$(mktemp -d)"
g2() { git -C "$t2" -c user.email=t@t -c user.name=t -c merge.conflictstyle=merge "$@"; }
git init -q -b main "$t2"
mkdir -p "$t2/spira"
printf '[added]\n"spira.alpha" = 1\n' > "$t2/$D"
g2 add -A; g2 commit -q -m base
for k in one two; do
  g2 checkout -q -b "$k" main
  echo "\"spira.key_$k\" = 1" >> "$t2/$D"
  g2 commit -q -am "add $k"
done
g2 checkout -q one
g2 merge -q --no-edit two >/dev/null 2>&1; rc=$?
is "without the attribute, concurrent delta additions conflict (positive control)" 1 "$rc"
g2 merge --abort >/dev/null 2>&1
cp "$HERE/../.gitattributes" "$t2/.gitattributes"
g2 add .gitattributes; g2 commit -q -m attrs
g2 merge -q --no-edit two >/dev/null 2>&1; rc=$?
is "with the repository's .gitattributes, delta additions merge cleanly" 0 "$rc"
merged="$(g2 show HEAD:$D 2>/dev/null)"
want "the delta merge keeps one's key" 'spira.key_one' "$merged"
want "the delta merge keeps two's key" 'spira.key_two' "$merged"
rm -rf "$t2"

tl_summary
