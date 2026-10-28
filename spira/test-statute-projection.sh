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
# covers: rule.sh cockpit-collect/src/* spira/law-synth.sh UC-operator-channel-43 G-07
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"
. "$HERE/testdb.sh"
testdb_require test-statute-projection
testdb_up statute-proj || bail "testdb_up failed"

TMP="$(mktemp -d)"
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM

RULE_SH="$(cd "$HERE/.." && pwd)/rule.sh"
STATUTE_DIR="$TMP/statutes"
[ -f "$RULE_SH" ] || bail "rule.sh not found at $RULE_SH"

# enact files a statute into a Part of the checkout it runs in, so every call runs inside a
# fixture checkout — never the tree under test.
LAW_FIX="$TMP/law-fix"
mkdir -p "$LAW_FIX/law" && git -C "$LAW_FIX" init -q
printf '[[part]]\nnumber = 1\nname = "Fixture"\nslug = "fixture"\nscope = "Fixture Part."\nslugs = []\n' > "$LAW_FIX/law/toc.toml"
rule_sh() { [ "$1" = enact ] && set -- "$@" --part 1; (cd "$LAW_FIX" && bash "$RULE_SH" "$@"); }

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
    tl_config SPIRA_STATUTE_DIR="$STATUTE_DIR" SPIRA_DB="$SPIRA_DB" SPIRA_WIKI_HOOK="$1"
    rule_sh enact "$2" --tool none --reason fixture "$3" 2>&1
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
WIKI_CL="$TMP/wiki-cl"; timeout 5 git clone -q "$WIKI_CL_ORIGIN" "$WIKI_CL" 2>/dev/null
git -C "$WIKI_CL" config user.email "test@spira"; git -C "$WIKI_CL" config user.name "test"
mkdir -p "$WIKI_CL/wiki/notes"
printf '# common law\n' > "$WIKI_CL/wiki/notes/common-law.md"
git -C "$WIKI_CL" add wiki/notes/common-law.md
git -C "$WIKI_CL" commit -qm "seed common-law"
timeout 5 git -C "$WIKI_CL" push -q origin main 2>/dev/null

# Stands in for law-synth.sh: appends to common-law.md the way a real regenerate would.
CL_HOOK="$TMP/cl-hook.sh"
printf '#!/bin/sh\nprintf "\\n### new entry\\n" >> "%s/wiki/notes/common-law.md"\necho "wrote common-law.md"\n' \
    "$WIKI_CL" > "$CL_HOOK"
chmod +x "$CL_HOOK"

run_enact_wiki() {
    tl_config SPIRA_STATUTE_DIR="$STATUTE_DIR" SPIRA_DB="$SPIRA_DB" SPIRA_WIKI_HOOK="$CL_HOOK" SPIRA_WIKI="$WIKI_CL"
    rule_sh enact "$1" --tool none --reason fixture "$2" 2>&1
}
run_retire_wiki() {
    tl_config SPIRA_STATUTE_DIR="$STATUTE_DIR" SPIRA_DB="$SPIRA_DB" SPIRA_WIKI_HOOK="$CL_HOOK" SPIRA_WIKI="$WIKI_CL"
    rule_sh retire "$1" 2>&1
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
tl_config SPIRA_STATUTE_DIR="$STATUTE_DIR" SPIRA_DB="$SPIRA_DB" SPIRA_WIKI_HOOK="$GOOD_HOOK" SPIRA_WIKI=""
out_cl_nowiki=$(rule_sh enact "sp-4fl2e-commit-nowiki" --tool none --reason fixture "No wiki canary." 2>&1)
want "commit: no SPIRA_WIKI prints manual-commit hint" \
    "Commit wiki/notes/common-law.md to replicate it off this box." "$out_cl_nowiki"

# ==========================================================================
# PAGE_N FIXTURE (UC-18, docs/test-plan/cockpit-observability.md): a small wiki tree with
# a committed common-law.md, 10 mock statutes. Built unconditionally — the cockpit-collect
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

run_rule() { tl_config SPIRA_STATUTE_DIR="$STATUTE_DIR" SPIRA_DB="$SPIRA_DB" SPIRA_WIKI_HOOK="$GOOD_HOOK"; rule_sh "$@" 2>&1; }

want "list names an enacted statute"     "law-sp-p0xyt-test-canary" "$(run_rule list)"
want "and reports a statute count"       "statutes in force"        "$(run_rule list)"

want "show prints the full text of an enacted statute" \
    "Canary statute for sp-p0xyt test suite." "$(run_rule show sp-p0xyt-test-canary)"
is "show on an unknown slug exits non-zero" "1" "$(run_rule show no-such-slug-xyz >/dev/null 2>&1; echo $?)"
want "and names the way to see what is in force" "rule.sh list" "$(run_rule show no-such-slug-xyz)"

# POSITIVE CONTROL FIRST: the statute is present, retire succeeds, and synth propagates
# the same way it does for enact.
out_retire="$(run_rule retire sp-p0xyt-pair-canary)"; rc_retire=$?
is   "retire on a real statute exits 0"       "0" "$rc_retire"
want "and says it forgot the key"             "forgot law-sp-p0xyt-pair-canary" "$out_retire"
want "and reminds not to leave a correction banner" "Do not leave a retired statute" "$out_retire"

is "retire removes it from list" "0" \
   "$(run_rule list | grep -c 'law-sp-p0xyt-pair-canary$' || true)"
is "and show on the retired slug now fails" "1" \
   "$(run_rule show sp-p0xyt-pair-canary >/dev/null 2>&1; echo $?)"

# NEGATIVE CONTROL: retiring a slug that was never enacted is refused, and refused BEFORE
# any bd write — the positive control above proves retire works at all, so a refusal here
# is the guard and not a broken command.
is "retire on an unknown slug is refused" "1" \
   "$(run_rule retire no-such-slug-abc >/dev/null 2>&1; echo $?)"
want "and names the missing statute" "no statute" "$(run_rule retire no-such-slug-abc)"

# retire with a failing synth hook: the write and the page are one operation, so the book
# is rolled back and nothing is written.
run_rule enact sp-p0xyt-retire-hookfail --tool none --reason fixture "Canary for the retire hook-failure path." >/dev/null 2>&1
tl_config SPIRA_STATUTE_DIR="$STATUTE_DIR" SPIRA_DB="$SPIRA_DB" SPIRA_WIKI_HOOK="$FAIL_HOOK"
out_retire_fail="$(rule_sh retire sp-p0xyt-retire-hookfail 2>&1)"
rc_retire_fail=$?
is   "retire with a failing hook exits non-zero" "1" "$rc_retire_fail"
want "and reports the rollback" "rolled back" "$out_retire_fail"
is "and the statute is still in the book" "0" \
   "$(run_rule show sp-p0xyt-retire-hookfail >/dev/null 2>&1; echo $?)"

# sp-pj5dp: enact is one operation with its page — from any cwd, and never half done.
tl_config SPIRA_STATUTE_DIR="$STATUTE_DIR" SPIRA_DB="$SPIRA_DB" SPIRA_WIKI_HOOK="$FAIL_HOOK"
out_e_fail="$(rule_sh enact sp-pj5dp-rollback --tool none --reason fixture "Rolled back canary." 2>&1)"
is "enact with a failing hook exits non-zero" "1" "$?"
want "and reports the rollback" "rolled back" "$out_e_fail"
is "and the new statute is not in the book" "1" \
   "$(run_rule show sp-pj5dp-rollback >/dev/null 2>&1; echo $?)"

run_rule enact sp-pj5dp-amend --tool none --reason fixture "Original text." >/dev/null 2>&1
tl_config SPIRA_STATUTE_DIR="$STATUTE_DIR" SPIRA_DB="$SPIRA_DB" SPIRA_WIKI_HOOK="$FAIL_HOOK"
rule_sh enact sp-pj5dp-amend --tool none --reason fixture "Amended text." >/dev/null 2>&1
want "a failed amendment restores the prior text" "Original text." "$(run_rule show sp-pj5dp-amend)"
nowant "and not the amendment" "Amended text." "$(run_rule show sp-pj5dp-amend)"

tl_config SPIRA_STATUTE_DIR="$STATUTE_DIR" SPIRA_DB="$SPIRA_DB" SPIRA_WIKI_HOOK="$MISSING_HOOK"
out_e_missing="$(rule_sh enact sp-pj5dp-nohook --tool none --reason fixture "No hook canary." 2>&1)"
is "enact with an unavailable hook exits non-zero" "1" "$?"
want "and says nothing was written" "nothing written" "$out_e_missing"
is "and the statute is not in the book" "1" \
   "$(run_rule show sp-pj5dp-nohook >/dev/null 2>&1; echo $?)"

tl_config SPIRA_STATUTE_DIR="$STATUTE_DIR" SPIRA_DB="$SPIRA_DB" SPIRA_WIKI_HOOK="./good-hook.sh"
cp "$GOOD_HOOK" "$LAW_FIX/good-hook.sh"
out_rel="$(rule_sh enact sp-pj5dp-relhook --tool none --reason fixture "Relative hook canary." 2>&1)"
is "a cwd-relative hook is refused even where it resolves" "1" "$?"
want "and the refusal names the path" "not an absolute path" "$out_rel"

tl_config SPIRA_STATUTE_DIR="$STATUTE_DIR" SPIRA_DB="$SPIRA_DB" SPIRA_WIKI_HOOK=""
out_cwd="$(rule_sh enact sp-pj5dp-cwd --tool none --reason fixture "Cwd canary." 2>&1)"
want "default hook resolves from a cwd with no law-synth.sh" "Statute is live" "$out_cwd"
nowant "and is not refused" "not executable" "$out_cwd"

# ==========================================================================
echo
echo "=== sp-dnrjw: enact rejects unrecognised arguments before touching the book ==="
# ==========================================================================
# `text="$*"` used to join every trailing argument into the statute text with no check, so
# a stray flag (a mistyped --dry-run) became law with no complaint (sp-dnrjw).

run_rule enact sp-dnrjw-enact-guard --tool none --reason fixture "Original text for the enact argument guard." >/dev/null 2>&1
before_guard="$(run_rule show sp-dnrjw-enact-guard)"

# NEGATIVE CONTROL: an unrecognised flag is refused, and the book is untouched.
out_badflag="$(run_rule enact sp-dnrjw-enact-guard --tool none --reason fixture "x" --whatever)"; rc_badflag=$?
is   "enact: unrecognised flag exits non-zero" "1" "$rc_badflag"
want "and names the offending argument"        "'--whatever'" "$out_badflag"
is   "and the statute is left unchanged" "$before_guard" "$(run_rule show sp-dnrjw-enact-guard)"

# NEGATIVE CONTROL: a second, unquoted text argument is refused the same way.
out_2ndtext="$(run_rule enact sp-dnrjw-enact-guard --tool none --reason fixture "x" "y")"; rc_2ndtext=$?
is   "enact: a second text argument exits non-zero" "1" "$rc_2ndtext"
want "and names the offending argument"             "'y'" "$out_2ndtext"
is   "and the statute is still left unchanged" "$before_guard" "$(run_rule show sp-dnrjw-enact-guard)"

# POSITIVE PAIR: one quoted text argument, with no flags, still enacts.
rc_goodtext=0; run_rule enact sp-dnrjw-enact-guard --tool none --reason fixture "Replacement text for the enact argument guard." >/dev/null || rc_goodtext=$?
is   "enact: one quoted text argument still exits 0" "0" "$rc_goodtext"
want "and the statute now reads the new text" \
    "Replacement text for the enact argument guard." "$(run_rule show sp-dnrjw-enact-guard)"

echo
echo "=== sp-dnrjw: enact prints the prior text before an overwrite ==="

before_overwrite="$(run_rule show sp-dnrjw-enact-guard)"
out_overwrite="$(run_rule enact sp-dnrjw-enact-guard --tool none --reason fixture "Second replacement for the overwrite-visibility test.")"
want "enact: overwrite prints the prior text" "$before_overwrite" "$out_overwrite"
want "and prints the new text" "Second replacement for the overwrite-visibility test." "$out_overwrite"

# POSITIVE CONTROL: enacting a brand-new slug prints no "already exists" overwrite banner.
nowant "enact: a brand-new slug prints no overwrite banner" "already exists" \
    "$(run_rule enact sp-dnrjw-enact-new --tool none --reason fixture "Brand new statute, never enacted before.")"

echo
echo "=== sp-dnrjw: enact --dry-run writes nothing ==="

before_dry="$(run_rule show sp-dnrjw-enact-guard)"
out_dry="$(run_rule enact sp-dnrjw-enact-guard --tool none --reason fixture "Text that --dry-run must not write." --dry-run)"; rc_dry=$?
is   "enact --dry-run exits 0"  "0" "$rc_dry"
want "and says DRY RUN"         "DRY RUN" "$out_dry"
is   "and the statute is left unchanged" "$before_dry" "$(run_rule show sp-dnrjw-enact-guard)"

echo
echo "=== enact names its tool; the audit finds statutes naming verbs that do not exist ==="

out_notool="$(run_rule enact tool-gate-none "A statute with nothing that can comply.")"; rc_notool=$?
is   "enact without --tool or --mechanism refuses" "1" "$rc_notool"
want "the refusal names --tool"      "--tool"      "$out_notool"
want "the refusal names --mechanism" "--mechanism" "$out_notool"
is   "and nothing was written" "1" "$(run_rule show tool-gate-none >/dev/null; echo $?)"
out_noreason="$(run_rule enact tool-gate-none --tool none "Judgement only.")"; rc_noreason=$?
is   "--tool none without a reason refuses" "1" "$rc_noreason"
out_badmech="$(run_rule enact tool-gate-none --mechanism "not a bead" "Judgement only.")"; rc_badmech=$?
is   "--mechanism that is not a bead id refuses" "1" "$rc_badmech"
out_mech="$(run_rule enact tool-gate-mech --mechanism sp-zzzzz "Mechanism pending statute.")"; rc_mech=$?
is   "--mechanism <bead> enacts" "0" "$rc_mech"
want "and records the mechanism bead" "sp-zzzzz" "$out_mech"

run_rule enact tool-audit-bad --tool bead.sh "Remove a bead with bead.sh nosuchverb and never anything else." >/dev/null 2>&1
run_rule enact tool-audit-ghost --tool x "Use ghost-tool-zzz.sh for it." >/dev/null 2>&1
run_rule enact tool-audit-good --tool bead.sh "File a bead with bead.sh file and no other way." >/dev/null 2>&1
out_audit="$(run_rule audit)"; rc_audit=$?
is     "audit exits 0 on findings" "0" "$rc_audit"
want   "audit flags a statute naming a verb the script lacks" "law-tool-audit-bad: bead.sh nosuchverb" "$out_audit"
want   "audit flags a statute naming a script the release lacks" "law-tool-audit-ghost: ghost-tool-zzz.sh" "$out_audit"
nowant "audit passes a statute naming a real verb" "law-tool-audit-good" "$out_audit"
for _k in tool-gate-mech tool-audit-bad tool-audit-ghost tool-audit-good; do run_rule retire "$_k" >/dev/null 2>&1; done

# ==========================================================================
echo
echo "=== rule.sh: default hook is the harness's own law-synth.sh (sp-fe3ee) ==="
# ==========================================================================

# rule.sh's default hook (SPIRA_WIKI_HOOK unset) is the harness's own spira/law-synth.sh —
# no file under brain/.claude involved. A fresh, uncommitted temp dir so this does not
# disturb WIKI_TMP's fixture state (its 10 committed headings are asserted on below).
WIKI_DEFAULT_HOOK="$TMP/wiki-default-hook"
mkdir -p "$WIKI_DEFAULT_HOOK"
tl_config SPIRA_STATUTE_DIR="$STATUTE_DIR" SPIRA_DB="$SPIRA_DB" SPIRA_WIKI="$WIKI_DEFAULT_HOOK" SPIRA_WIKI_HOOK=""
out_default_hook=$(rule_sh enact "sp-fe3ee-default-hook-test" --tool none --reason fixture "Default hook canary statute." 2>&1)
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
echo "=== cockpit-collect statute_keys: SP_STATUTE_SKEW (no-wiki case) ==="

run_statute_keys() { tl_config SPIRA_STATUTE_DIR="$STATUTE_DIR" SPIRA_DB="$SPIRA_DB" SPIRA_WIKI="${1:-}"; cockpit-collect probe statute 2>/dev/null; }

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
    # batch-job: fixture bd call against the suite's throwaway store
    "$SPIRA_BD" -C "$SPIRA_DB" remember --key "law-statute-keys-mismatch-test-$i" \
        "Mismatch test statute $i." >/dev/null 2>&1 || true
done

out_mismatch=$(run_statute_keys "$WIKI_TMP")
want   "statute_keys: mismatch → SP_STATUTE_SKEW contains MISMATCH" "MISMATCH" "$out_mismatch"
nowant "statute_keys: mismatch → SP_STATUTE_SKEW is not OK"         "SKEW=OK"  "$out_mismatch"

# Add 8 more law- entries so db has 11 (>= 10/2 = 5, in fact exceeds page).
for i in $(seq 4 11); do
    # batch-job: fixture bd call against the suite's throwaway store
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
