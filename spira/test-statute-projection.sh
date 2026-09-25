#!/usr/bin/env bash
#
# test-statute-projection.sh — statute projection integrity: rule.sh and law-synth.sh
#   guard against silent synthesis failures and wrong-database overwrites.
#
# WHAT IS UNDER TEST
# ------------------
# Three defects discovered 2026-09-11 (sp-p0xyt):
#
#   1. rule.sh called synth() and discarded its exit code, printing "Statute is live" even
#      when synthesis had failed. A missing or non-executable hook returned 0 silently.
#
#   2. law-synth.sh guarded the database path but not its content: a store with .beads and
#      zero law- memories passed the check and overwrote 112 statutes with 3.
#
#   3. law-cron.sh's detection was correct but wrote to a log file nobody reads. The fix is
#      SP_STATUTE_SKEW in cockpit.env (cockpit.sh statute_keys), surfaced where the operator
#      actually sees it.
#
# PAIRS (law-absence-needs-a-positive-control): every negative case is paired with a positive
# case so silence from the negative case looks like failure, not peace.
#
# REAL DB (law-prefer-the-real-dependency): tests run against a real bd fixture database via
# testdb.sh, not a stub that models only the surface we remember.
#
# covers: rule.sh spira/cockpit.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testdb.sh"
testdb_require test-statute-projection
testdb_up statute-proj || { echo "testdb_up failed" >&2; exit 1; }

pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }
exits0() { local rc; "$@" >/dev/null 2>&1; rc=$?; [ $rc -eq 0 ] && ok "$1 (exits 0)" || bad "$1 (exits 0)" "rc=$rc"; }
exitsnot0() { local rc; "$@" >/dev/null 2>&1; rc=$?; [ $rc -ne 0 ] && ok "$1 (exits non-zero)" || bad "$1 (exits non-zero)" "got rc=0"; }

TMP="$(mktemp -d)"
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM

# Rule.sh needs SPIRA_DB and SPIRA_WIKI_HOOK. SPIRA_DB is set by testdb_up.
RULE_SH="$(cd "$HERE/.." && pwd)/rule.sh"
[ -f "$RULE_SH" ] || { echo "SKIP rule.sh not found at $RULE_SH" >&2; exit 77; }

# law-synth.sh lives in brain's .claude/; locate it through BRAIN or SPIRA_WIKI.
# BRAIN is set when this suite runs inside the brain session or under brain's own test runner.
# SPIRA_WIKI is the configured wiki path in spira.conf — same location from the harness side.
LAW_SYNTH_SH=""
for _try_brain in "${BRAIN:-}" "${SPIRA_WIKI:-}"; do
    [ -n "$_try_brain" ] || continue
    _candidate="$_try_brain/.claude/law-synth.sh"
    if [ -f "$_candidate" ]; then
        LAW_SYNTH_SH="$_candidate"
        break
    fi
done
[ -f "${LAW_SYNTH_SH:-}" ] || {
    printf 'SKIP law-synth.sh not reachable (BRAIN and SPIRA_WIKI not set, or law-synth.sh absent)\n' >&2
    # Only skip the law-synth section; rule.sh and cockpit tests still run.
    LAW_SYNTH_SH=""
}

echo "test-statute-projection.sh"

# ==========================================================================
echo
echo "=== rule.sh: synth failure propagation ==="
# ==========================================================================

# A working hook — will be created in $TMP.
GOOD_HOOK="$TMP/good-hook.sh"
printf '#!/bin/sh\necho "synth: ran ok" >&2\n' > "$GOOD_HOOK"
chmod +x "$GOOD_HOOK"

# A failing hook.
FAIL_HOOK="$TMP/fail-hook.sh"
printf '#!/bin/sh\necho "synth: failed" >&2\nexit 1\n' > "$FAIL_HOOK"
chmod +x "$FAIL_HOOK"

# Non-executable hook path (exists but not +x).
NON_EXEC_HOOK="$TMP/non-exec-hook.sh"
printf '#!/bin/sh\necho "should not run" >&2\n' > "$NON_EXEC_HOOK"
# Deliberately do NOT chmod +x.

# Missing hook path.
MISSING_HOOK="$TMP/does-not-exist.sh"

run_enact() {
    # Runs rule.sh enact with a controlled hook and db.
    local hook="$1" key="$2" text="$3"
    SPIRA_DB="$SPIRA_DB" SPIRA_WIKI_HOOK="$hook" bash "$RULE_SH" enact "$key" "$text" 2>&1
}

# POSITIVE CONTROL: good hook → exits 0 and prints "Statute is live".
# Plant a law before asserting — so the db has one law- entry (law-synth requires >=1).
out_pc=$(run_enact "$GOOD_HOOK" "sp-p0xyt-test-canary" "Canary statute for sp-p0xyt test suite."); rc_pc=$?
if [ $rc_pc -eq 0 ]; then
    ok "positive control: good hook exits 0"
else
    bad "positive control: good hook exits 0" "rc=$rc_pc"
fi
want "positive control: prints 'Statute is live'" "Statute is live" "$out_pc"
nowant "positive control: does not print 'NOT regenerated'" "NOT regenerated" "$out_pc"

# NEGATIVE CONTROL 1: failing hook → exits non-zero, does NOT print "Statute is live".
out_fail=$(run_enact "$FAIL_HOOK" "sp-p0xyt-fail-test" "Another canary."); rc_fail=$?
if [ $rc_fail -ne 0 ]; then
    ok "failing hook: exits non-zero"
else
    bad "failing hook: exits non-zero" "got rc=0"
fi
nowant "failing hook: does NOT print 'Statute is live'"    "Statute is live"   "$out_fail"
want   "failing hook: prints 'NOT regenerated'"            "NOT regenerated"   "$out_fail"

# NEGATIVE CONTROL 2: missing hook → exits non-zero, error message.
out_missing=$(run_enact "$MISSING_HOOK" "sp-p0xyt-missing-test" "Missing hook canary."); rc_missing=$?
if [ $rc_missing -ne 0 ]; then
    ok "missing hook: exits non-zero"
else
    bad "missing hook: exits non-zero" "got rc=0"
fi
nowant "missing hook: does NOT print 'Statute is live'" "Statute is live" "$out_missing"
# Either "not executable" or "is not set" covers both sub-cases.
if [[ "$out_missing" == *"not executable"* ]] || [[ "$out_missing" == *"not set"* ]] || [[ "$out_missing" == *"NOT regenerated"* ]]; then
    ok "missing hook: prints diagnostic"
else
    bad "missing hook: prints diagnostic" "got: $out_missing"
fi

# NEGATIVE CONTROL 3: non-executable hook → exits non-zero.
out_noexec=$(run_enact "$NON_EXEC_HOOK" "sp-p0xyt-noexec-test" "Non-exec hook canary."); rc_noexec=$?
if [ $rc_noexec -ne 0 ]; then
    ok "non-executable hook: exits non-zero"
else
    bad "non-executable hook: exits non-zero" "got rc=0"
fi
nowant "non-executable hook: does NOT print 'Statute is live'" "Statute is live" "$out_noexec"

# POSITIVE PAIR for controls 2 and 3: a real hook still succeeds after those checks.
out_pair=$(run_enact "$GOOD_HOOK" "sp-p0xyt-pair-canary" "Positive pair for missing/non-exec tests."); rc_pair=$?
if [ $rc_pair -eq 0 ]; then
    ok "positive pair: good hook after bad cases still exits 0"
else
    bad "positive pair: good hook after bad cases still exits 0" "rc=$rc_pair"
fi
want "positive pair: prints 'Statute is live'" "Statute is live" "$out_pair"

# ==========================================================================
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
echo "=== law-synth.sh: wrong-database guard ==="
# ==========================================================================

if [ -z "$LAW_SYNTH_SH" ]; then
    echo "  SKIP (law-synth.sh not reachable)"
else
    run_synth() {
        SPIRA_DB="$SPIRA_DB" BRAIN="$WIKI_TMP" bash "$LAW_SYNTH_SH" 2>&1
    }
    run_synth_override() {
        SPIRA_DB="$SPIRA_DB" BRAIN="$WIKI_TMP" LAW_SYNTH_OVERRIDE=1 bash "$LAW_SYNTH_SH" 2>&1
    }

    # POSITIVE CONTROL: pointed at real book (fixture db has law- entries from enact tests above).
    out_synth_ok=$(run_synth); rc_synth_ok=$?
    if [ $rc_synth_ok -eq 0 ]; then
        ok "law-synth: pointed at real book exits 0"
    else
        bad "law-synth: pointed at real book exits 0" "rc=$rc_synth_ok output=$out_synth_ok"
    fi
    want "law-synth: real book writes output" "wrote" "$out_synth_ok"

    # NEGATIVE CONTROL 1: empty database (reset to fresh, which has no law- memories).
    testdb_reset || { bad "testdb_reset" "failed"; }

    out_synth_empty=$(run_synth); rc_synth_empty=$?
    if [ $rc_synth_empty -ne 0 ]; then
        ok "law-synth: empty database exits non-zero"
    else
        bad "law-synth: empty database exits non-zero" "got rc=0"
    fi
    want   "law-synth: empty database: mentions refusing" "refusing" "$out_synth_empty"
    nowant "law-synth: empty database: does NOT write"    "wrote"    "$out_synth_empty"

    # Verify the committed page was NOT touched.
    page_after_empty="$(git -C "$WIKI_TMP" diff HEAD -- wiki/notes/common-law.md 2>/dev/null)"
    if [ -z "$page_after_empty" ]; then
        ok "law-synth: empty database: committed page untouched"
    else
        bad "law-synth: empty database: committed page untouched" "page was modified"
    fi

    # NEGATIVE CONTROL 2: pointed at a database with far fewer laws than committed page.
    # Add 3 law- entries (committed page has 10, so 3 < 10/2 = 5).
    for i in 1 2 3; do
        "$SPIRA_BD" -C "$SPIRA_DB" remember --key "law-synth-floor-test-$i" \
            "Floor test statute $i." >/dev/null 2>&1 || true
    done

    out_synth_floor=$(run_synth); rc_synth_floor=$?
    if [ $rc_synth_floor -ne 0 ]; then
        ok "law-synth: floor check (3 vs 10) exits non-zero"
    else
        bad "law-synth: floor check (3 vs 10) exits non-zero" "got rc=0"
    fi
    want "law-synth: floor check: mentions refusing" "refusing" "$out_synth_floor"

    # POSITIVE CONTROL 2: same db but with LAW_SYNTH_OVERRIDE=1 → succeeds.
    out_synth_force=$(run_synth_override); rc_synth_force=$?
    if [ $rc_synth_force -eq 0 ]; then
        ok "law-synth: LAW_SYNTH_OVERRIDE=1 overrides floor check"
    else
        bad "law-synth: LAW_SYNTH_OVERRIDE=1 overrides floor check" "rc=$rc_synth_force output=$out_synth_force"
    fi
fi

# ==========================================================================
echo
echo "=== cockpit.sh statute_keys: SP_STATUTE_SKEW ==="
# ==========================================================================

COCKPIT_SH="$HERE/cockpit.sh"

run_statute_keys() {
    # Run statute_keys via the cockpit.sh statute subcommand.
    SPIRA_DB="$SPIRA_DB" SPIRA_WIKI="${1:-}" bash "$COCKPIT_SH" statute 2>/dev/null
}

# NEGATIVE CONTROL: SPIRA_WIKI unset → all ?
out_no_wiki=$(run_statute_keys "")
want   "statute_keys: no wiki → SP_STATUTE_DB_N=?"    "SP_STATUTE_DB_N=?"    "$out_no_wiki"
want   "statute_keys: no wiki → SP_STATUTE_PAGE_N=?"  "SP_STATUTE_PAGE_N=?"  "$out_no_wiki"
want   "statute_keys: no wiki → SP_STATUTE_SKEW=?"    "SP_STATUTE_SKEW=?"    "$out_no_wiki"

# UC-18: the MISMATCH/OK cases below seed their own db state instead of reusing the
# law-synth section's (which only runs when LAW_SYNTH_SH — a reachable brain checkout —
# is found), so SP_STATUTE_PAGE_N/SKEW are exercised against WIKI_TMP whether or not
# brain is reachable in this environment.
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
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
