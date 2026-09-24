#!/usr/bin/env bash
#
# test-law-synth.sh — law-synth.sh's wrong-database guard, and the cockpit skew it feeds.
#
# HOST-ONLY, AND SAID SO (G-12). law-synth.sh lives in the brain wiki checkout (BRAIN or
# SPIRA_WIKI), which a container running only the harness does not have. Before this suite,
# that absence made half of test-statute-projection.sh's assertions vanish silently behind an
# `if`, still reporting green — a container run and a full run of that file printed the same
# "all passed" for a different number of assertions. `skip` makes the absence a COUNTED skip
# (TAP skip-all, exit 77) instead: reported as exactly what it is, a T4 case this environment
# cannot run, not folded into a pass.
#
# WHAT IS UNDER TEST (sp-p0xyt)
# ------------------------------
#   law-synth.sh guarded the database path but not its content: a store with .beads and zero
#   law- memories passed the check and overwrote 112 statutes with 3. cockpit.sh's
#   SP_STATUTE_SKEW is where the operator actually sees the mismatch (law-cron.sh wrote a log
#   file nobody read).
#
# PAIRS (law-absence-needs-a-positive-control): every negative case is paired with a positive
# case so silence from the negative case looks like failure, not peace.
#
# tier: T4
# covers: rule.sh spira/cockpit.sh UC-operator-channel-43
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

# BRAIN is set when this suite runs inside the brain session or under brain's own test
# runner; SPIRA_WIKI is the configured wiki path in spira.conf — same location from the
# harness side. Neither is guaranteed in a container that carries only the harness checkout.
LAW_SYNTH_SH=""
for _try_brain in "${BRAIN:-}" "${SPIRA_WIKI:-}"; do
    [ -n "$_try_brain" ] || continue
    _candidate="$_try_brain/.claude/law-synth.sh"
    [ -f "$_candidate" ] && { LAW_SYNTH_SH="$_candidate"; break; }
done
[ -n "$LAW_SYNTH_SH" ] || skip "law-synth.sh not reachable (BRAIN and SPIRA_WIKI unset, or law-synth.sh absent) — this is a T4 host case, reported as skipped rather than silently folded into green"

. "$HERE/testdb.sh"
testdb_require test-law-synth
testdb_up law-synth || bail "testdb_up failed"

TMP="$(mktemp -d)"
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM

WIKI_TMP="$TMP/wiki"
mkdir -p "$WIKI_TMP/wiki/notes"
git -C "$WIKI_TMP" init -q 2>/dev/null
git -C "$WIKI_TMP" config user.email "test@spira" 2>/dev/null
git -C "$WIKI_TMP" config user.name "test" 2>/dev/null

{
    echo "---"; echo "type: note"; echo "updated: 2026-01-01"; echo "---"; echo ""
    for i in $(seq 1 10); do
        echo "### Statute $i"; echo ""; echo "Text of statute $i."; echo ""
    done
} > "$WIKI_TMP/wiki/notes/common-law.md"
git -C "$WIKI_TMP" add wiki/notes/common-law.md 2>/dev/null
git -C "$WIKI_TMP" commit -q -m "test: baseline common-law" 2>/dev/null

run_synth()          { SPIRA_DB="$SPIRA_DB" BRAIN="$WIKI_TMP" bash "$LAW_SYNTH_SH" 2>&1; }
run_synth_override() { SPIRA_DB="$SPIRA_DB" BRAIN="$WIKI_TMP" LAW_SYNTH_OVERRIDE=1 bash "$LAW_SYNTH_SH" 2>&1; }

echo "=== law-synth.sh: wrong-database guard ==="

# Plant one law- entry so the fixture db isn't empty for the positive control.
"$SPIRA_BD" -C "$SPIRA_DB" remember --key "law-synth-seed" "Seed statute." >/dev/null 2>&1

# POSITIVE CONTROL: pointed at real book.
out_synth_ok=$(run_synth); rc_synth_ok=$?
is   "law-synth: pointed at real book exits 0" "0" "$rc_synth_ok"
want "law-synth: real book writes output"      "wrote" "$out_synth_ok"

# NEGATIVE CONTROL 1: empty database (reset to fresh, which has no law- memories).
testdb_reset || bad "testdb_reset" "failed"
out_synth_empty=$(run_synth); rc_synth_empty=$?
if [ "$rc_synth_empty" -ne 0 ]; then ok "law-synth: empty database exits non-zero"; else bad "law-synth: empty database exits non-zero" "got rc=0"; fi
want   "law-synth: empty database: mentions refusing" "refusing" "$out_synth_empty"
nowant "law-synth: empty database: does NOT write"    "wrote"    "$out_synth_empty"

page_after_empty="$(git -C "$WIKI_TMP" diff HEAD -- wiki/notes/common-law.md 2>/dev/null)"
is "law-synth: empty database: committed page untouched" "" "$page_after_empty"

# NEGATIVE CONTROL 2: pointed at a database with far fewer laws than the committed page.
for i in 1 2 3; do
    "$SPIRA_BD" -C "$SPIRA_DB" remember --key "law-synth-floor-test-$i" \
        "Floor test statute $i." >/dev/null 2>&1 || true
done
out_synth_floor=$(run_synth); rc_synth_floor=$?
if [ "$rc_synth_floor" -ne 0 ]; then ok "law-synth: floor check (3 vs 10) exits non-zero"; else bad "law-synth: floor check (3 vs 10) exits non-zero" "got rc=0"; fi
want "law-synth: floor check: mentions refusing" "refusing" "$out_synth_floor"

# POSITIVE CONTROL 2: same db but with LAW_SYNTH_OVERRIDE=1 → succeeds.
out_synth_force=$(run_synth_override); rc_synth_force=$?
is "law-synth: LAW_SYNTH_OVERRIDE=1 overrides floor check" "0" "$rc_synth_force"

echo
echo "=== cockpit.sh statute_keys: SP_STATUTE_SKEW against a real mismatch ==="

run_statute_keys() { SPIRA_DB="$SPIRA_DB" SPIRA_WIKI="$WIKI_TMP" bash "$HERE/cockpit.sh" statute 2>/dev/null; }

# POSITIVE CONTROL: fixture db has 3 law- entries (from the floor test above); the committed
# page has 10 ### headings. 3 < 10/2 → MISMATCH.
out_mismatch=$(run_statute_keys)
want   "statute_keys: mismatch → SP_STATUTE_SKEW contains MISMATCH" "MISMATCH" "$out_mismatch"
nowant "statute_keys: mismatch → SP_STATUTE_SKEW is not OK"         "SKEW=OK"  "$out_mismatch"

# Add 8 more law- entries so the db has 11 (>= 10/2 = 5, and exceeds the page).
for i in $(seq 4 11); do
    "$SPIRA_BD" -C "$SPIRA_DB" remember --key "law-synth-ok-test-$i" \
        "OK test statute $i." >/dev/null 2>&1 || true
done
out_ok=$(run_statute_keys)
want   "statute_keys: counts match → SP_STATUTE_SKEW=OK"   "SKEW=OK"  "$out_ok"
nowant "statute_keys: counts match → not MISMATCH"          "MISMATCH" "$out_ok"

db_n="$(printf '%s' "$out_ok" | grep '^SP_STATUTE_DB_N=' | cut -d= -f2)"
page_n="$(printf '%s' "$out_ok" | grep '^SP_STATUTE_PAGE_N=' | cut -d= -f2)"
case "$db_n" in ''|*[!0-9]*) bad "SP_STATUTE_DB_N is numeric" "got: $db_n" ;;
    *) ok "SP_STATUTE_DB_N is numeric ($db_n)" ;; esac
case "$page_n" in ''|*[!0-9]*) bad "SP_STATUTE_PAGE_N is numeric" "got: $page_n" ;;
    *) ok "SP_STATUTE_PAGE_N is numeric ($page_n)" ;; esac

tl_summary
