#!/usr/bin/env bash
#
# test-ask-suit.sh — ask.sh suit: filing validation, label presence, and verdict execution
#
#   ./test-ask-suit.sh
#
# WHAT THIS GUARDS. A lawsuit is a challenge to a statute in force: it must be refused
# when the slug does not exist, must embed the slug on the bead as a label, and the
# three verdicts (uphold/retire/amend) must produce the right effect in the statute book.
#
# law-a-regression-test-must-be-seen-to-fail: run against the unfixed tree to confirm
# each assertion fails there, then run against the fix to confirm it passes.
#
# covers: cockpit/ask.sh spira/answers.py cockpit/panel/src/model.rs cockpit/panel/src/store.rs
# set -uo pipefail
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
COCKPIT="$(cd "$HERE/../cockpit" && pwd -P)"
REPO="$(dirname "$HERE")"
. "$HERE/testdb.sh"
testdb_require test-ask-suit
testdb_up ask-suit || { echo "testdb_up failed"; exit 1; }

pass=0; fail=0
ok()   { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()  { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()   { [ "$2" = "$3" ] && ok "$1" || bad "$1" "expected [$2] got [$3]"; }
want() { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
wont() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did NOT want [$2] in [$3]"; }

TMP="$(mktemp -d)"
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM

export COCKPIT_DB="$SPIRA_DB"
BD="${TESTDB_BD:-bd}"

bdt()  { "$BD" -C "$SPIRA_DB" "$@"; }
ask()  { COCKPIT_DB="$SPIRA_DB" bash "$COCKPIT/ask.sh" "$@"; }
# rule.sh REFUSES TO REPORT SUCCESS WITHOUT A WIKI HOOK, and it is right to: enacting a
# statute that never reaches the wiki page leaves the operator told "Statute is live" while
# the page sits stale (sp-p0xyt). With SPIRA_WIKI_HOOK unset it writes the statute, says so,
# and exits 1 — so every `rule enact` in this suite reported "rule.sh enact failed" while the
# assertions immediately after it confirmed the statute was in force. Seven FAILs, none of
# them about ask.sh.
#
# A STUB, NOT A TOLERATED NON-ZERO EXIT. Accepting rc=1 here would also accept a genuine
# enact failure, which is the one thing these fixtures must not do. The stub makes the hook
# succeed so the exit code still means what it says. Whether rule.sh correctly REFUSES on a
# bad or missing hook is test-statute-projection.sh's subject, and is asserted there against
# the real law-synth.sh; this suite's subject is ask.sh suit.
WIKI_HOOK_STUB="$TMP/law-synth-stub.sh"
printf '#!/usr/bin/env bash\nexit 0\n' > "$WIKI_HOOK_STUB"
chmod +x "$WIKI_HOOK_STUB"
rule() { SPIRA_DB="$SPIRA_DB" SPIRA_WIKI_HOOK="$WIKI_HOOK_STUB" bash "$REPO/rule.sh" "$@"; }

count_beads() {
    "$BD" -C "$SPIRA_DB" list --all --limit 0 --json 2>/dev/null \
        | python3 -c 'import json,sys; d=json.load(sys.stdin); print(len(d if isinstance(d,list) else d.get("issues",[])))' 2>/dev/null || echo 0
}

bead_labels() { # <bead-id>
    "$BD" -C "$SPIRA_DB" list --all --limit 0 --json 2>/dev/null \
        | python3 -c "
import json,sys
target=sys.argv[1]
rows=json.load(sys.stdin)
rows=rows if isinstance(rows,list) else rows.get('issues',[])
for r in rows:
    if r.get('id')==target:
        print(','.join(r.get('labels') or []))
        break
" "$1" 2>/dev/null || echo ""
}

echo "test-ask-suit.sh"

# ======================================================================================
echo
echo "positive control — count_beads can detect a creation"
# ======================================================================================
before="$(count_beads)"
bdt create --title "positive-control" --type task >/dev/null 2>&1 || true
after="$(count_beads)"
[ "$after" -gt "$before" ] \
    && ok "count_beads moves on creation (positive control)" \
    || bad "count_beads moves on creation (positive control)" "before=$before after=$after"

testdb_reset

# ======================================================================================
echo
echo "filing against a slug NOT in force: refused, non-zero, no bead created"
# ======================================================================================
before="$(count_beads)"
out="$(ask suit "statute-that-does-not-exist" --why "testing" 2>&1)"; rc=$?
after="$(count_beads)"
[ "$rc" -ne 0 ] \
    && ok  "suit against absent slug exits non-zero" \
    || bad "suit against absent slug exits non-zero" "got rc=$rc"
want "suit against absent slug names the slug" "statute-that-does-not-exist" "$out"
is   "suit against absent slug creates no bead" "$before" "$after"

# ======================================================================================
echo
echo "filing against a slug IN force: creates bead with right labels"
# ======================================================================================
# Enact a test statute so there is something to challenge.
TEST_SLUG="suit-test-statute"
rule enact "$TEST_SLUG" "This statute exists only to be challenged by the ask-suit test. Not real law." >/dev/null 2>&1 \
    || { bad "enact test statute" "rule.sh enact failed"; }

before="$(count_beads)"
out="$(ask suit "$TEST_SLUG" --why "It is too broad for a test statute." 2>&1)"; rc=$?
after="$(count_beads)"

is  "suit with valid slug exits 0"   "0" "$rc"
[ "$after" -gt "$before" ] \
    && ok  "suit with valid slug creates a bead" \
    || bad "suit with valid slug creates a bead" "before=$before after=$after"

# Extract the bead id from the output: "suit [sp-xxx] against law-<slug>"
suit_id="$(printf '%s\n' "$out" | grep -oE '\[[a-z]+-[a-z0-9]+\]' | tr -d '[]' | head -1)"
[ -n "$suit_id" ] \
    && ok  "suit output carries a bead id" \
    || bad "suit output carries a bead id" "out=[$out]"

if [ -n "$suit_id" ]; then
    labels="$(bead_labels "$suit_id")"
    want "bead carries ask-suit label"              "ask-suit"                   "$labels"
    want "bead carries overseer label"              "overseer"                   "$labels"
    want "bead carries statute:law-<slug> label"   "statute:law-${TEST_SLUG}"   "$labels"
fi

# Title must name the statute for readability in the panel.
if [ -n "$suit_id" ]; then
    title="$("$BD" -C "$SPIRA_DB" list --all --limit 0 --json 2>/dev/null \
        | python3 -c "
import json,sys
target=sys.argv[1]
rows=json.load(sys.stdin)
rows=rows if isinstance(rows,list) else rows.get('issues',[])
for r in rows:
    if r.get('id')==target:
        print(r.get('title',''))
        break
" "$suit_id" 2>/dev/null || echo "")"
    want "suit title names the statute" "law-${TEST_SLUG}" "$title"
fi

# ======================================================================================
echo
echo "retire verdict: statute absent from rule.sh list"
# ======================================================================================
# Enact a second statute for the retire test (we retire it, not the one the suit bead holds).
RETIRE_SLUG="suit-retire-target"
rule enact "$RETIRE_SLUG" "Retire target: this statute will be retired by the ask-suit test verdict." >/dev/null 2>&1 \
    || { bad "enact retire target" "rule.sh enact failed"; }

# Confirm the statute is present before the test.
rule_list="$(rule list 2>/dev/null)"
want "retire target is present before test" "law-${RETIRE_SLUG}" "$rule_list"

# File a suit, then close it with retire verdict + run rule.sh retire.
retire_id="$(ask suit "$RETIRE_SLUG" --why "Retiring this for the test." 2>&1 \
    | grep -oE '\[[a-z]+-[a-z0-9]+\]' | tr -d '[]' | head -1)"
if [ -n "$retire_id" ]; then
    # Simulate the panel's retire verdict: run rule.sh first, then close.
    rule retire "$RETIRE_SLUG" >/dev/null 2>&1 \
        && bdt close "$retire_id" --reason "retire: law-${RETIRE_SLUG} retired" --force >/dev/null 2>&1 \
        && ok "retire verdict ran without error" \
        || bad "retire verdict ran without error" "rule retire or bead close failed"

    rule_list_after="$(rule list 2>/dev/null)"
    wont "statute absent after retire"  "law-${RETIRE_SLUG}"  "$rule_list_after"
else
    bad "retire verdict: could not create suit bead" "bead creation failed"
    bad "statute absent after retire"                "skipped"  "bead creation failed"
fi

# ======================================================================================
echo
echo "amend verdict: new text in force, old text gone"
# ======================================================================================
AMEND_SLUG="suit-amend-target"
OLD_TEXT="Original text for the amend test. This is what existed before the lawsuit."
NEW_TEXT="Amended text for the amend test. The lawsuit replaced the original statement."
rule enact "$AMEND_SLUG" "$OLD_TEXT" >/dev/null 2>&1 \
    || { bad "enact amend target" "rule.sh enact failed"; }

old_text_in_store="$(rule show "$AMEND_SLUG" 2>/dev/null)"
want "old text in force before amend" "Original text" "$old_text_in_store"

amend_id="$(ask suit "$AMEND_SLUG" --why "The original was too vague." 2>&1 \
    | grep -oE '\[[a-z]+-[a-z0-9]+\]' | tr -d '[]' | head -1)"
if [ -n "$amend_id" ]; then
    # Simulate the panel's amend verdict: run rule.sh enact (replaces), then close.
    rule enact "$AMEND_SLUG" "$NEW_TEXT" >/dev/null 2>&1 \
        && bdt close "$amend_id" --reason "amend: law-${AMEND_SLUG} amended" --force >/dev/null 2>&1 \
        && ok "amend verdict ran without error" \
        || bad "amend verdict ran without error" "rule enact or bead close failed"

    new_text_in_store="$(rule show "$AMEND_SLUG" 2>/dev/null)"
    want "new text in force after amend"  "Amended text"    "$new_text_in_store"
    wont "old text gone after amend"      "Original text"   "$new_text_in_store"
else
    bad "amend verdict: could not create suit bead" "bead creation failed"
    bad "new text in force after amend"             "skipped" "bead creation failed"
    bad "old text gone after amend"                 "skipped" "bead creation failed"
fi

# ======================================================================================
echo
echo "uphold verdict: statute unchanged, bead closed"
# ======================================================================================
UPHOLD_SLUG="suit-uphold-target"
UPHOLD_TEXT="Uphold target: this statute must survive the lawsuit unchanged."
rule enact "$UPHOLD_SLUG" "$UPHOLD_TEXT" >/dev/null 2>&1 \
    || { bad "enact uphold target" "rule.sh enact failed"; }

uphold_id="$(ask suit "$UPHOLD_SLUG" --why "Testing uphold path." 2>&1 \
    | grep -oE '\[[a-z]+-[a-z0-9]+\]' | tr -d '[]' | head -1)"
if [ -n "$uphold_id" ]; then
    # Simulate uphold: just close, statute unchanged.
    bdt close "$uphold_id" --reason "uphold" --force >/dev/null 2>&1 \
        && ok "uphold: bead closed without error" \
        || bad "uphold: bead closed without error" "bd close failed"

    uphold_text="$(rule show "$UPHOLD_SLUG" 2>/dev/null)"
    want "uphold: statute text unchanged" "Uphold target" "$uphold_text"
else
    bad "uphold verdict: could not create suit bead" "bead creation failed"
    bad "uphold: statute text unchanged"             "skipped" "bead creation failed"
fi

# ======================================================================================
echo
echo "answers.py: suit verdict renders distinctly from RYAN ANSWERED"
# ======================================================================================
ANSWER_SLUG="suit-answers-test"
rule enact "$ANSWER_SLUG" "Answers-test statute. Used to verify the watcher renders suit verdicts." >/dev/null 2>&1 \
    || { bad "enact answers-test statute" "rule.sh enact failed"; }

ans_id="$(ask suit "$ANSWER_SLUG" --why "Testing watcher output." 2>&1 \
    | grep -oE '\[[a-z]+-[a-z0-9]+\]' | tr -d '[]' | head -1)"
if [ -n "$ans_id" ]; then
    # Close as a retire verdict (using the operator actor so answers.py sees it).
    rule retire "$ANSWER_SLUG" >/dev/null 2>&1
    BEADS_ACTOR="${SPIRA_OPERATOR_ACTOR:-operator}" \
        "$BD" -C "$SPIRA_DB" close "$ans_id" \
        --reason "retire: law-${ANSWER_SLUG} retired" --force >/dev/null 2>&1 \
        || bad "answers-test: close as operator" "bd close failed"

    # Simulate answers.py output. answers.py reads from stdin (a JSON list of beads).
    ANSWERS="$HERE/answers.py"
    raw="$("$BD" -C "$SPIRA_DB" list --all --limit 0 --json 2>/dev/null | sed -n '/^[[{]/,$p')"

    # Pre-seed cursors at a past timestamp so the verdict (closed after this) IS new.
    printf '{"ts":"2020-01-01T00:00:00Z","seen":[]}' > "$TMP/vc"
    printf '{"ts":"2020-01-01T00:00:00Z","seen":[]}' > "$TMP/cc"
    watcher_out="$(printf '%s' "$raw" | python3 "$ANSWERS" \
        "bd=${TESTDB_BD:-bd}" "db=$SPIRA_DB" \
        "ask_label=${SPIRA_ASK_LABEL:-needs-operator}" \
        "operator_actor=${SPIRA_OPERATOR_ACTOR:-operator}" \
        "operator=ryan" \
        "verdict_cursor=$TMP/vc" "comment_cursor=$TMP/cc" \
        format=monitor 2>/dev/null)"

    wont "suit retire verdict is NOT rendered as RYAN ANSWERED" "RYAN ANSWERED" "$watcher_out"
    want "suit retire verdict renders RETIRED"                  "RETIRED"        "$watcher_out"
else
    bad "answers-test: could not create suit bead" "bead creation failed"
    bad "suit retire verdict is NOT rendered as RYAN ANSWERED" "skipped" "bead creation failed"
    bad "suit retire verdict renders RETIRED"                  "skipped" "bead creation failed"
fi

# ======================================================================================
echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
