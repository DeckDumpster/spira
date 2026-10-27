#!/usr/bin/env bash
#
# test-mender.sh — the mender is a lane of its own, sized by config, and its brief, its
# verbs and the broker's allow table name the same set.
#
# tier: T1
# covers: spira/runes/mender.* spira/conf.d/SPIRA_MENDER_* spira/conf.d/SPIRA_LANES mender/src/* spira-lc/src/work.rs work/src/lib.rs
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
HARNESS="$(cd "$HERE/.." && pwd -P)"
. "$HERE/testlib.sh"

echo "test-mender.sh"
RUNE="$HERE/runes/mender.rune"
BRIEF="$HERE/runes/mender.md"
rune_var() { ( set +u; . "$RUNE" 2>/dev/null; eval "printf '%s' \"\${$1:-}\"" ); }

echo; echo "LANE: its own, outside the builder pool"
want "the rune is a lane" "mender" "$(rune_var RUNE_LANE)"
want "the lane is declared" "mender" "$(cat "$HERE/conf.d/SPIRA_LANES")"
want "the ceiling is the config key, not a literal" "3" "$(SPIRA_MENDER_MAX=3 rune_var RUNE_MAX_CONCURRENT)"
want "the ceiling defaults to eight" "8" "$(env -u SPIRA_MENDER_MAX bash -c 'set +u; . "$0"; printf %s "$RUNE_MAX_CONCURRENT"' "$RUNE")"
want "Opus" "opus" "$(rune_var RUNE_MODEL)"
for k in SPIRA_MENDER_MAX SPIRA_MENDER_DEADLINE_SECS SPIRA_MENDER_LABEL; do
    [ -f "$HERE/conf.d/$k" ] && ok "$k is a registered key" || bad "$k is a registered key" "no spira/conf.d/$k"
done
want "the deadline defaults to five minutes" "SPIRA_MENDER_DEADLINE_SECS:=300" "$(cat "$HERE/conf.d/SPIRA_MENDER_DEADLINE_SECS")"
want "the session wall outlasts the deadline" "420" "$(env -u SPIRA_MENDER_DEADLINE_SECS bash -c 'set +u; . "$0"; printf %s "$RUNE_TIMEOUT_SECONDS"' "$RUNE")"
want "a pinned deadline moves the wall" "620" "$(SPIRA_MENDER_DEADLINE_SECS=500 bash -c 'set +u; . "$0"; printf %s "$RUNE_TIMEOUT_SECONDS"' "$RUNE")"

echo; echo "PARTITION: the summons label alone, never plan work"
want "the predicate reads the mender label" 'SPIRA_MENDER_LABEL' "$(grep '^RUNE_LABELS=' "$RUNE")"
lack() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }
lack "no plan label" 'plan' "$(grep '^RUNE_LABELS=' "$RUNE")"

echo; echo "INTEGRATE: the mender owns the summons kind, the builder cannot claim it"
want "the brief carries the integrate remit" "## An integration" "$(grep -o '^## An integration' "$BRIEF")"
want "the mender may submit a branch" "SUBMITTED" "$(rune_var RUNE_WORKING_EXITS | grep -o SUBMITTED)"
lack "the integrate summons is not on the plan label" 'integrate' "$(grep '^RUNE_LABELS=' "$HERE/runes/builder.rune")"

echo; echo "VERBS: brief, binary and allow table agree"
USAGE="$(grep -o '^const USAGE: &str = "[^"]*"' "$HARNESS/mender/src/main.rs")"
verbs="start pack diff bead members failures passes verdicts landing triaged diagnose outcome"
for v in $verbs; do
    want "the binary's usage names $v" "$v" "$USAGE"
    want "the brief names $v" "work mender $v" "$(grep -o "work mender $v\b" "$BRIEF" | head -1)"
    want "the broker allows only the mender $v" "(\"mender $v\", &[\"mender\"])" "$(grep -F "(\"mender $v\"" "$HARNESS/spira-lc/src/work.rs")"
done
for o in mended environment flaky return unsure; do
    want "the brief names the $o outcome" "\`$o\`" "$(grep -o "\`$o\`" "$BRIEF" | head -1)"
done
lack "the brief asks for no filing" "work file " "$(cat "$BRIEF")"

echo; echo "POSITIVE CONTROL: the verb check can fail"
[ -z "$(grep -F '("mender bisect"' "$HARNESS/spira-lc/src/work.rs")" ] && ok "an unlisted verb is not in the allow table" || bad "an unlisted verb is not in the allow table" "mender bisect is listed"
[ -n "$(grep -F '("mender pack"' "$HARNESS/spira-lc/src/work.rs")" ] && ok "a listed verb is found by the same grep" || bad "a listed verb is found by the same grep" "the grep cannot find mender pack"

tl_summary
