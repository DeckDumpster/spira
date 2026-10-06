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

# chamber-core-check.sh reads SPIRA_DB directly from its own process environment
# (DB="${SPIRA_DB:?...}") — it never sources conf.sh, so this is a plain env prefix, not
# registered-key config.
out="$(SPIRA_DB="$SPIRA_DB" "$CHECK" "$TMP/good" 2>&1)"; rc=$?
is "every declared slug resolving passes" 0 "$rc"

out="$(SPIRA_DB="$SPIRA_DB" "$CHECK" "$TMP/bad" 2>&1)"; rc=$?
is   "an unresolvable slug fails"        1 "$rc"
want "and names it"                      "law-retired-and-renamed" "$out"
case "$out" in *"names law-chamber-core-a,"*) bad "a resolvable slug is not reported" "$out" ;; *) ok "a resolvable slug is not reported" ;; esac

out="$(SPIRA_DB="$TMP/nowhere" "$CHECK" "$TMP/good" 2>&1)"; rc=$?
is "an unreadable store is refused, not passed" 2 "$rc"

# THE SHIPPED SEEDS COVER EVERY CHAMBER'S CORE, not just the builder's (sp-crr3n widens
# sp-6ka75's check). A fresh install holds only what spira/statutes/ ships, so a core slug
# any fayth ends up declaring but the seeds lack makes that fresh-install persona refuse to
# start ("core statute(s) declared but not found") pre-session. Static: no store needed.
#
# A FAYTH THAT DECLARES NO FAYTH_STATUTE_CORE OF ITS OWN still has one: aeon's run.rs falls
# back to SPIRA_STATUTE_CORE's default (lib.sh: "unset, it gets $SPIRA_STATUTE_CORE"), which
# is ops/groomer/maechen/batcher/spike's real core in production — the gap acceptance phase B
# caught for ops. Resolve that same default once, from spira/conf.d/SPIRA_STATUTE_CORE's seed
# line, so a fayth with no override is checked against what it actually gets at summon.
global_core="$(sed -n 's/.*SPIRA_STATUTE_CORE:=\(.*\)}".*/\1/p' "$HERE/conf.d/SPIRA_STATUTE_CORE" | tail -1)"
[ -n "$global_core" ] || bad "conf.d/SPIRA_STATUTE_CORE's default parses (positive control)" "none parsed"

total_checked=0
all_unshipped=""
for f in "$HERE"/chamber/*.fayth; do
    name="$(basename "$f" .fayth)"
    own="$(sed -n 's/^FAYTH_STATUTE_CORE="\(.*\)"/\1/p' "$f" | tail -1)"
    core="${own:-$global_core}"
    core="$(tr ', ' '\n\n' <<<"$core" | sed '/^$/d')"
    for c in $core; do
        total_checked=$((total_checked + 1))
        [ -f "$HERE/statutes/$c.txt" ] || all_unshipped="$all_unshipped $name:$c"
    done
done
[ "$total_checked" -gt 0 ] || bad "at least one chamber fayth resolves a core slug (positive control)" "none parsed"
is "every chamber fayth's core statute ships in spira/statutes/" "" "${all_unshipped# }"

tl_summary
