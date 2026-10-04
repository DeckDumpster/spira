#!/usr/bin/env bash
#
# test-chamber-core-check.sh — chamber-core-check.sh fails on a FAYTH_STATUTE_CORE slug that is
# not a live memory, and passes when every one resolves.
#
# covers: spira/chamber-core-check.sh spira/chamber/*.fayth
# tier: T1
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"
. "$HERE/testdb.sh"
testdb_require test-chamber-core-check
testdb_up chamber-core || bail "testdb_up failed"
TMP="$(mktemp -d)"
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM

CHECK="$HERE/chamber-core-check.sh"
"$SPIRA_BD" -C "$SPIRA_DB" remember --key law-chamber-core-a "A." >/dev/null 2>&1
"$SPIRA_BD" -C "$SPIRA_DB" remember --key law-chamber-core-b "B." >/dev/null 2>&1

mkdir -p "$TMP/good" "$TMP/bad"
printf 'FAYTH_NAME=x\nFAYTH_STATUTE_CORE="law-chamber-core-a, law-chamber-core-b"\n' > "$TMP/good/x.fayth"
printf 'FAYTH_NAME=y\n' > "$TMP/good/y.fayth"
printf 'FAYTH_NAME=z\nFAYTH_STATUTE_CORE="law-chamber-core-a,law-retired-and-renamed"\n' > "$TMP/bad/z.fayth"

out="$(SPIRA_DB="$SPIRA_DB" "$CHECK" "$TMP/good" 2>&1)"; rc=$?
is "every declared slug resolving passes" 0 "$rc"

out="$(SPIRA_DB="$SPIRA_DB" "$CHECK" "$TMP/bad" 2>&1)"; rc=$?
is   "an unresolvable slug fails"        1 "$rc"
want "and names it"                      "law-retired-and-renamed" "$out"
case "$out" in *"names law-chamber-core-a,"*) bad "a resolvable slug is not reported" "$out" ;; *) ok "a resolvable slug is not reported" ;; esac

out="$(SPIRA_DB="$TMP/nowhere" "$CHECK" "$TMP/good" 2>&1)"; rc=$?
is "an unreadable store is refused, not passed" 2 "$rc"

# THE SHIPPED SEEDS COVER THE BUILDER'S CORE. A fresh install holds only what spira/statutes/
# ships, so a core slug the builder declares but the seeds lack makes every fresh-install
# builder refuse to start (acceptance 37183437236, sp-6ka75). Static: no store needed.
# The other fayths are sp-crr3n's to bring under this check.
core="$(sed -n 's/^FAYTH_STATUTE_CORE="\(.*\)"/\1/p' "$HERE/chamber/builder.fayth" | tr ', ' '\n\n' | sed '/^$/d')"
[ -n "$core" ] || bad "builder.fayth declares a core statute (positive control)" "none parsed"
unshipped=""
for c in $core; do [ -f "$HERE/statutes/$c.txt" ] || unshipped="$unshipped $c"; done
is "every builder core statute ships in spira/statutes/" "" "${unshipped# }"

tl_summary
