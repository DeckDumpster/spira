#!/usr/bin/env bash
#
# test-statute-projection.sh — rule.sh: synth-failure propagation, and the full command set
#   (enact/retire/list/show) against a real statute book.
#
# WHAT IS UNDER TEST
# ------------------
# Three defects discovered 2026-09-11 (sp-p0xyt), the first two here:
#
#   1. rule.sh called synth() and discarded its exit code, printing "Statute is live" even
#      when synthesis had failed. A missing or non-executable hook returned 0 silently.
#
#   2. law-synth.sh guarded the database path but not its content: a store with .beads and
#      zero law- memories passed the check and overwrote 112 statutes with 3. This half lives
#      in test-law-synth.sh, against the harness's own spira/law-synth.sh (sp-fe3ee).
#
# G-07 — `rule.sh retire`, `list` and `show` were untested; only `enact` was. CLAUDE.md
# prescribes `retire` for a superseded statute, so the gap is a real one: nothing proved the
# command a maintainer is told to run for that case actually works.
#
# PAIRS (law-absence-needs-a-positive-control): every negative case is paired with a positive
# case so silence from the negative case looks like failure, not peace.
#
# REAL DB (law-prefer-the-real-dependency): tests run against a real bd fixture database via
# testdb.sh, not a stub that models only the surface we remember.
#
# tier: T2
# covers: rule.sh spira/cockpit.sh spira/law-synth.sh UC-operator-channel-43 G-07
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"
. "$HERE/testdb.sh"
testdb_require test-statute-projection
testdb_up statute-proj || bail "testdb_up failed"

TMP="$(mktemp -d)"
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM

RULE_SH="$(cd "$HERE/.." && pwd)/rule.sh"
[ -f "$RULE_SH" ] || bail "rule.sh not found at $RULE_SH"

echo "=== rule.sh: synth failure propagation ==="

GOOD_HOOK="$TMP/good-hook.sh"
printf '#!/bin/sh\necho "synth: ran ok" >&2\n' > "$GOOD_HOOK"
chmod +x "$GOOD_HOOK"

FAIL_HOOK="$TMP/fail-hook.sh"
printf '#!/bin/sh\necho "synth: failed" >&2\nexit 1\n' > "$FAIL_HOOK"
chmod +x "$FAIL_HOOK"

NON_EXEC_HOOK="$TMP/non-exec-hook.sh"
printf '#!/bin/sh\necho "should not run" >&2\n' > "$NON_EXEC_HOOK"
# Deliberately do NOT chmod +x.

MISSING_HOOK="$TMP/does-not-exist.sh"

run_enact() {   # run_enact <hook> <key> <text>
    SPIRA_DB="$SPIRA_DB" SPIRA_WIKI_HOOK="$1" bash "$RULE_SH" enact "$2" "$3" 2>&1
}

# POSITIVE CONTROL: good hook → exits 0 and prints "Statute is live".
out_pc=$(run_enact "$GOOD_HOOK" "sp-p0xyt-test-canary" "Canary statute for sp-p0xyt test suite."); rc_pc=$?
is   "positive control: good hook exits 0"                   "0" "$rc_pc"
want "positive control: prints 'Statute is live'"            "Statute is live" "$out_pc"
nowant "positive control: does not print 'NOT regenerated'"  "NOT regenerated" "$out_pc"

# NEGATIVE CONTROL 1: failing hook → exits non-zero, does NOT print "Statute is live".
out_fail=$(run_enact "$FAIL_HOOK" "sp-p0xyt-fail-test" "Another canary."); rc_fail=$?
if [ "$rc_fail" -ne 0 ]; then ok "failing hook: exits non-zero"; else bad "failing hook: exits non-zero" "got rc=0"; fi
nowant "failing hook: does NOT print 'Statute is live'"    "Statute is live"   "$out_fail"
want   "failing hook: prints 'NOT regenerated'"            "NOT regenerated"   "$out_fail"

# NEGATIVE CONTROL 2: missing hook → exits non-zero, error message.
out_missing=$(run_enact "$MISSING_HOOK" "sp-p0xyt-missing-test" "Missing hook canary."); rc_missing=$?
if [ "$rc_missing" -ne 0 ]; then ok "missing hook: exits non-zero"; else bad "missing hook: exits non-zero" "got rc=0"; fi
nowant "missing hook: does NOT print 'Statute is live'" "Statute is live" "$out_missing"
if [[ "$out_missing" == *"not executable"* ]] || [[ "$out_missing" == *"not set"* ]] || [[ "$out_missing" == *"NOT regenerated"* ]]; then
    ok "missing hook: prints diagnostic"
else
    bad "missing hook: prints diagnostic" "got: $out_missing"
fi

# NEGATIVE CONTROL 3: non-executable hook → exits non-zero.
out_noexec=$(run_enact "$NON_EXEC_HOOK" "sp-p0xyt-noexec-test" "Non-exec hook canary."); rc_noexec=$?
if [ "$rc_noexec" -ne 0 ]; then ok "non-executable hook: exits non-zero"; else bad "non-executable hook: exits non-zero" "got rc=0"; fi
nowant "non-executable hook: does NOT print 'Statute is live'" "Statute is live" "$out_noexec"

# POSITIVE PAIR for controls 2 and 3: a real hook still succeeds after those checks.
out_pair=$(run_enact "$GOOD_HOOK" "sp-p0xyt-pair-canary" "Positive pair for missing/non-exec tests."); rc_pair=$?
is   "positive pair: good hook after bad cases still exits 0" "0" "$rc_pair"
want "positive pair: prints 'Statute is live'"                "Statute is live" "$out_pair"

echo
echo "=== rule.sh: commits wiki/notes/common-law.md itself (sp-4fl2e) ==="
# ==========================================================================
# Without this, synth()'s write to common-law.md sits dirty in the shared wiki checkout
# until some other actor's session exits and sweeps it up under the wrong authorship.
# rule.sh must commit that one path itself, immediately, under a `law: enact/retire <key>`
# message — a real git checkout is required to prove the tree ends up clean, not a stub.

WIKI_CL_ORIGIN="$TMP/wiki-cl.git"; git init -q --bare -b main "$WIKI_CL_ORIGIN"
WIKI_CL="$TMP/wiki-cl"; git clone -q "$WIKI_CL_ORIGIN" "$WIKI_CL" 2>/dev/null
git -C "$WIKI_CL" config user.email "test@spira"; git -C "$WIKI_CL" config user.name "test"
mkdir -p "$WIKI_CL/wiki/notes"
printf '# common law\n' > "$WIKI_CL/wiki/notes/common-law.md"
git -C "$WIKI_CL" add wiki/notes/common-law.md
git -C "$WIKI_CL" commit -qm "seed common-law"
git -C "$WIKI_CL" push -q origin main 2>/dev/null

# Stands in for law-synth.sh: appends to common-law.md the way a real regenerate would.
CL_HOOK="$TMP/cl-hook.sh"
printf '#!/bin/sh\nprintf "\\n### new entry\\n" >> "%s/wiki/notes/common-law.md"\necho "wrote common-law.md"\n' \
    "$WIKI_CL" > "$CL_HOOK"
chmod +x "$CL_HOOK"

run_enact_wiki() {
    SPIRA_DB="$SPIRA_DB" SPIRA_WIKI_HOOK="$CL_HOOK" SPIRA_WIKI="$WIKI_CL" \
        bash "$RULE_SH" enact "$1" "$2" 2>&1
}
run_retire_wiki() {
    SPIRA_DB="$SPIRA_DB" SPIRA_WIKI_HOOK="$CL_HOOK" SPIRA_WIKI="$WIKI_CL" \
        bash "$RULE_SH" retire "$1" 2>&1
}

# POSITIVE CONTROL: enact commits common-law.md itself and leaves the tree clean.
out_cl_enact=$(run_enact_wiki "sp-4fl2e-commit-test" "Canary for rule.sh self-commit."); rc_cl_enact=$?
if [ $rc_cl_enact -eq 0 ]; then
    ok "commit: enact exits 0"
else
    bad "commit: enact exits 0" "rc=$rc_cl_enact output=$out_cl_enact"
fi
want "commit: enact prints committed confirmation" "wiki/notes/common-law.md committed" "$out_cl_enact"
is "commit: enact leaves wiki checkout clean" "" \
    "$(git -C "$WIKI_CL" status --short --untracked-files=all 2>/dev/null)"
is "commit: enact commit message names the statute" "law: enact law-sp-4fl2e-commit-test" \
    "$(git -C "$WIKI_CL" log --format=%s -1 -- wiki/notes/common-law.md 2>/dev/null)"
is "commit: enact commit touches only common-law.md" "wiki/notes/common-law.md" \
    "$(git -C "$WIKI_CL" show --name-only --format='' HEAD 2>/dev/null)"

# POSITIVE CONTROL: retire commits common-law.md itself too.
out_cl_retire=$(run_retire_wiki "sp-4fl2e-commit-test"); rc_cl_retire=$?
if [ $rc_cl_retire -eq 0 ]; then
    ok "commit: retire exits 0"
else
    bad "commit: retire exits 0" "rc=$rc_cl_retire output=$out_cl_retire"
fi
is "commit: retire leaves wiki checkout clean" "" \
    "$(git -C "$WIKI_CL" status --short --untracked-files=all 2>/dev/null)"
is "commit: retire commit message names the statute" "law: retire law-sp-4fl2e-commit-test" \
    "$(git -C "$WIKI_CL" log --format=%s -1 -- wiki/notes/common-law.md 2>/dev/null)"

# NEGATIVE CONTROL: no SPIRA_WIKI → no auto-commit attempted, manual-commit hint printed
# (unchanged behaviour — this is the "rule.sh: synth failure propagation" section's own
# GOOD_HOOK, which never touches a wiki checkout at all).
out_cl_nowiki=$(SPIRA_DB="$SPIRA_DB" SPIRA_WIKI_HOOK="$GOOD_HOOK" SPIRA_WIKI="" \
    bash "$RULE_SH" enact "sp-4fl2e-commit-nowiki" "No wiki canary." 2>&1)
want "commit: no SPIRA_WIKI prints manual-commit hint" \
    "Commit wiki/notes/common-law.md to replicate it off this box." "$out_cl_nowiki"

# ==========================================================================
# PAGE_N FIXTURE (UC-18, docs/test-plan/cockpit-observability.md): a small wiki tree with
# a committed common-law.md, 10 mock statutes. Built unconditionally — the cockpit.sh
# statute_keys section below reads it directly and must not depend on a reachable brain
# checkout, only the law-synth.sh section further down does.
# ==========================================================================
WIKI_TMP="$TMP/wiki"
mkdir -p "$WIKI_TMP/wiki/notes"
git -C "$WIKI_TMP" init -q 2>/dev/null
git -C "$WIKI_TMP" config user.email "test@spira" 2>/dev/null
git -C "$WIKI_TMP" config user.name "test" 2>/dev/null

{
    echo "---"
    echo "type: note"
    echo "updated: 2026-01-01"
    echo "---"
    echo ""
    for i in $(seq 1 10); do
        echo "### Statute $i"
        echo ""
        echo "Text of statute $i."
        echo ""
    done
} > "$WIKI_TMP/wiki/notes/common-law.md"
git -C "$WIKI_TMP" add wiki/notes/common-law.md 2>/dev/null
git -C "$WIKI_TMP" commit -q -m "test: baseline common-law" 2>/dev/null

# ==========================================================================
echo
echo "=== G-07: rule.sh list, show and retire ==="
# ==========================================================================

run_rule() { SPIRA_DB="$SPIRA_DB" SPIRA_WIKI_HOOK="$GOOD_HOOK" bash "$RULE_SH" "$@" 2>&1; }

want "list names an enacted statute"     "law-sp-p0xyt-test-canary" "$(run_rule list)"
want "and reports a statute count"       "statutes in force"        "$(run_rule list)"

want "show prints the full text of an enacted statute" \
    "Canary statute for sp-p0xyt test suite." "$(run_rule show sp-p0xyt-test-canary)"
is "show on an unknown slug exits non-zero" "1" "$(run_rule show no-such-slug-xyz >/dev/null 2>&1; echo $?)"
want "and names the way to see what is in force" "rule.sh list" "$(run_rule show no-such-slug-xyz)"

# POSITIVE CONTROL FIRST: the statute is present, retire succeeds, and synth propagates
# the same way it does for enact.
out_retire="$(run_rule retire sp-p0xyt-fail-test)"; rc_retire=$?
is   "retire on a real statute exits 0"       "0" "$rc_retire"
want "and says it forgot the key"             "forgot law-sp-p0xyt-fail-test" "$out_retire"
want "and reminds not to leave a correction banner" "Do not leave a retired statute" "$out_retire"

is "retire removes it from list" "0" \
   "$(run_rule list | grep -c 'law-sp-p0xyt-fail-test$' || true)"
is "and show on the retired slug now fails" "1" \
   "$(run_rule show sp-p0xyt-fail-test >/dev/null 2>&1; echo $?)"

# NEGATIVE CONTROL: retiring a slug that was never enacted is refused, and refused BEFORE
# any bd write — the positive control above proves retire works at all, so a refusal here
# is the guard and not a broken command.
is "retire on an unknown slug is refused" "1" \
   "$(run_rule retire no-such-slug-abc >/dev/null 2>&1; echo $?)"
want "and names the missing statute" "no statute" "$(run_rule retire no-such-slug-abc)"

# retire with a failing synth hook: the book is still changed, the hook failure is reported,
# and the command exits non-zero about it — the same contract enact holds.
run_rule enact sp-p0xyt-retire-hookfail "Canary for the retire hook-failure path." >/dev/null 2>&1
out_retire_fail="$(SPIRA_DB="$SPIRA_DB" SPIRA_WIKI_HOOK="$FAIL_HOOK" bash "$RULE_SH" retire sp-p0xyt-retire-hookfail 2>&1)"
rc_retire_fail=$?
is   "retire with a failing hook exits non-zero" "1" "$rc_retire_fail"
want "the book write is reported to have succeeded" "removed from the book" "$out_retire_fail"
want "and the wiki page is reported NOT regenerated" "NOT regenerated" "$out_retire_fail"
is "and the statute is gone from the book regardless" "1" \
   "$(run_rule show sp-p0xyt-retire-hookfail >/dev/null 2>&1; echo $?)"

# ==========================================================================
echo
echo "=== rule.sh: default hook is the harness's own law-synth.sh (sp-fe3ee) ==="
# ==========================================================================

# rule.sh's default hook (SPIRA_WIKI_HOOK unset) is the harness's own spira/law-synth.sh —
# no file under brain/.claude involved. A fresh, uncommitted temp dir so this does not
# disturb WIKI_TMP's fixture state (its 10 committed headings are asserted on below).
WIKI_DEFAULT_HOOK="$TMP/wiki-default-hook"
mkdir -p "$WIKI_DEFAULT_HOOK"
out_default_hook=$(SPIRA_DB="$SPIRA_DB" SPIRA_WIKI="$WIKI_DEFAULT_HOOK" \
    bash "$RULE_SH" enact "sp-fe3ee-default-hook-test" "Default hook canary statute." 2>&1)
rc_default_hook=$?
if [ $rc_default_hook -eq 0 ]; then
    ok "rule.sh: default hook (no SPIRA_WIKI_HOOK) exits 0"
else
    bad "rule.sh: default hook (no SPIRA_WIKI_HOOK) exits 0" "rc=$rc_default_hook output=$out_default_hook"
fi
want   "rule.sh: default hook: statute is live"     "Statute is live" "$out_default_hook"
nowant "rule.sh: default hook: no brain path named" "brain/.claude"   "$out_default_hook"
[ -f "$WIKI_DEFAULT_HOOK/wiki/notes/common-law.md" ] \
    && ok "rule.sh: default hook rendered common-law.md" \
    || bad "rule.sh: default hook rendered common-law.md" "file missing"

echo
echo "=== cockpit.sh statute_keys: SP_STATUTE_SKEW (no-wiki case) ==="

COCKPIT_SH="$HERE/cockpit.sh"
run_statute_keys() { SPIRA_DB="$SPIRA_DB" SPIRA_WIKI="${1:-}" bash "$COCKPIT_SH" statute 2>/dev/null; }

# NEGATIVE CONTROL: SPIRA_WIKI unset → all ? (the WIKI-dependent MISMATCH/OK cases live in
# test-law-synth.sh, which needs a wiki checkout and reports its absence as a skip).
out_no_wiki=$(run_statute_keys "")
want "statute_keys: no wiki → SP_STATUTE_DB_N=?"    "SP_STATUTE_DB_N=?"    "$out_no_wiki"
want "statute_keys: no wiki → SP_STATUTE_PAGE_N=?"  "SP_STATUTE_PAGE_N=?"  "$out_no_wiki"
want "statute_keys: no wiki → SP_STATUTE_SKEW=?"    "SP_STATUTE_SKEW=?"    "$out_no_wiki"

# UC-18: the MISMATCH/OK cases below seed their own db state instead of reusing
# test-law-synth.sh's, so SP_STATUTE_PAGE_N/SKEW are exercised against WIKI_TMP here
# independent of whatever that suite leaves its own fixture db holding.
testdb_reset || { bad "testdb_reset (statute_keys fixture)" "failed"; }

# 3 law- entries against WIKI_TMP's 10 ### headings: 3 < 10/2 → MISMATCH.
for i in 1 2 3; do
    "$SPIRA_BD" -C "$SPIRA_DB" remember --key "law-statute-keys-mismatch-test-$i" \
        "Mismatch test statute $i." >/dev/null 2>&1 || true
done

out_mismatch=$(run_statute_keys "$WIKI_TMP")
want   "statute_keys: mismatch → SP_STATUTE_SKEW contains MISMATCH" "MISMATCH" "$out_mismatch"
nowant "statute_keys: mismatch → SP_STATUTE_SKEW is not OK"         "SKEW=OK"  "$out_mismatch"

# Add 8 more law- entries so db has 11 (>= 10/2 = 5, in fact exceeds page).
for i in $(seq 4 11); do
    "$SPIRA_BD" -C "$SPIRA_DB" remember --key "law-statute-keys-ok-test-$i" \
        "OK test statute $i." >/dev/null 2>&1 || true
done

# POSITIVE CONTROL: db has 11, page has 10 → OK (11 >= 10/2 and not drastically below).
out_ok=$(run_statute_keys "$WIKI_TMP")
want   "statute_keys: counts match → SP_STATUTE_SKEW=OK"   "SKEW=OK" "$out_ok"
nowant "statute_keys: counts match → not MISMATCH"          "MISMATCH" "$out_ok"

# SP_STATUTE_DB_N and SP_STATUTE_PAGE_N must both be numeric.
db_n="$(printf '%s' "$out_ok" | grep '^SP_STATUTE_DB_N=' | cut -d= -f2)"
page_n="$(printf '%s' "$out_ok" | grep '^SP_STATUTE_PAGE_N=' | cut -d= -f2)"
case "$db_n" in ''|*[!0-9]*) bad "SP_STATUTE_DB_N is numeric" "got: $db_n" ;;
    *) ok "SP_STATUTE_DB_N is numeric ($db_n)" ;; esac
case "$page_n" in ''|*[!0-9]*) bad "SP_STATUTE_PAGE_N is numeric" "got: $page_n" ;;
    *) ok "SP_STATUTE_PAGE_N is numeric ($page_n)" ;; esac
is "SP_STATUTE_PAGE_N=10 (from the WIKI_TMP fixture)" "10" "$page_n"

echo
tl_summary
