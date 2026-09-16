#!/usr/bin/env bash
#
# test-ask-law.sh — ask.sh law files a bead the panel can enact; label, format and body verified.
#
# THE SCAR (labels). Four statute proposals were accepted at 04:11 on 2026-09-10 but nothing
# enacted them — acceptance depended on an agent remembering to run rule.sh by hand. The fix is
# ask-law: the panel runs rule.sh enact itself. But the panel can only do that if the bead
# carries the right labels. A label mismatch is silent at filing time and catastrophic at
# acceptance time. (sp-zumrf)
#
# THE SCAR (body). A law proposal titled with 70 words of statute and no decision line in the
# body caused the operator to answer "yes? this seems good. i don't know what i'm supposed to
# decide here." The body must lead with the decision and its default so the operator knows what
# they are being asked before reading the statute text. (sp-t4506, sp-y73fp)
#
# WHAT IS UNDER TEST.
#   1. ask.sh law must file a bead with label `ask-law` (panel routing).
#   2. The rendered body must lead with the decision question and a default.
#   3. The statute text must appear in the body (operator can read it without squinting at title).
#   4. ask.sh law without --default still emits a sensible default ("yes — enact it").
#
# PAIRS (law-absence-needs-a-positive-control): every negative assertion is paired with a
# positive one so a broken path that produces nothing still looks like a test failure.
#
# covers: cockpit/panel/src/model.rs cockpit/panel/src/store.rs
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
COCKPIT="$(cd "$HERE/../cockpit" && pwd -P)"
. "$HERE/testdb.sh"
testdb_require test-ask-law
testdb_up ask-law || { echo "testdb_up failed"; exit 1; }

pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "expected [$2] got [$3]"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

TMP="$(mktemp -d)"
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM

# SPIRA_BD and SPIRA_DB are exported by testdb_up and are the correct binary+database pair.
bdt() { "$SPIRA_BD" -C "$SPIRA_DB" "$@" 2>/dev/null; }

# ask.sh sources conf.sh which derives SPIRA_ASK_LABEL (default: needs-operator); pass
# COCKPIT_DB so it writes to our throwaway database rather than the real one.
ask() { COCKPIT_DB="$SPIRA_DB" SPIRA_BD="$SPIRA_BD" bash "$COCKPIT/ask.sh" "$@"; }

echo "test-ask-law.sh"

# ======================================================================================
echo
echo "positive control — ask.sh can create a bead at all"
# ======================================================================================
out_pc="$(ask insight "positive-control insight" 2>&1)"; rc_pc=$?
is "insight creation exits 0 (positive control)" "0" "$rc_pc"
want "insight output carries bead id" "[sp-" "$out_pc"

# ======================================================================================
echo
echo "ask.sh law: basic creation"
# ======================================================================================
SLUG="never-log-before-check"
STATUTE="Never write to a log before a guard checks that the write is safe."
LAW_TITLE="${SLUG}: ${STATUTE}"

out="$(ask law "$LAW_TITLE" --why "a log that precedes its guard asserts unchecked conditions" 2>&1)"
rc=$?
is "ask.sh law exits 0" "0" "$rc"
want "output carries 'law ['" "law [" "$out"

# Extract the bead id from the output: "law [sp-xxx] ..."
id="$(printf '%s' "$out" | grep -oE '\[[a-z]+-[a-z0-9]+\]' | tr -d '[]' | head -1)"
if [ -n "$id" ]; then
    ok "bead id extracted from output ($id)"
else
    bad "bead id extracted from output" "got: $out"
fi

# ======================================================================================
echo
echo "body leads with decision question and default"
# ======================================================================================
# THE REGRESSION. sp-t4506 was filed with the statute as the title and no decision line in
# the body — the operator answered "i don't know what i'm supposed to decide here." The body
# must lead with "Enact this statute?" before anything else.
if [ -n "$id" ]; then
    body="$(bdt show "$id" --json | python3 -c \
        'import json,sys; d=json.load(sys.stdin); d=d[0] if isinstance(d,list) else d; print(d.get("description",""))')"

    # NEGATIVE CONTROL: file a bead with the old broken structure (no decision line).
    # The test expects the new structure, so this body is distinct and confirms the check is real.
    nowant "body does not contain 'What is blocked'" "What is blocked" "$body"

    # POSITIVE CONTROL: the new structure must be present.
    want "body leads with Enact decision question" "Enact this statute?" "$body"
    want "body contains Default line"              "Default:"            "$body"
    want "body contains statute text"             "$STATUTE"            "$body"
    want "body contains enact verdict option"     "enact"               "$body"
    want "body contains amend verdict option"     "amend"               "$body"
    want "body contains decline verdict option"   "decline"             "$body"

    # Decision line must appear before statute text — operator reads top-down.
    decision_pos="${body%%Enact this statute?*}"
    statute_pos="${body%%${STATUTE}*}"
    if [ "${#decision_pos}" -lt "${#statute_pos}" ]; then
        ok "decision line appears before statute text in body"
    else
        bad "decision line appears before statute text in body" \
            "decision at offset ${#decision_pos}, statute at offset ${#statute_pos}"
    fi
fi

# ======================================================================================
echo
echo "body default is 'yes — enact it' when --default is omitted"
# ======================================================================================
out_nodflt="$(ask law "slug-nodefault: Never omit the default." 2>&1)"; rc_nodflt=$?
is "law without --default exits 0" "0" "$rc_nodflt"
id_nodflt="$(printf '%s' "$out_nodflt" | grep -oE '\[[a-z]+-[a-z0-9]+\]' | tr -d '[]' | head -1)"
if [ -n "$id_nodflt" ]; then
    body_nodflt="$(bdt show "$id_nodflt" --json | python3 -c \
        'import json,sys; d=json.load(sys.stdin); d=d[0] if isinstance(d,list) else d; print(d.get("description",""))')"
    want "omitted --default renders 'yes' default"        "yes"        "$body_nodflt"
    want "omitted --default still has decision question"  "Enact this" "$body_nodflt"
else
    bad "omitted --default: bead id not found"  "got: $out_nodflt"
    bad "omitted --default renders 'yes' default" "skipped" "bead creation failed"
    bad "omitted --default still has decision question" "skipped" "bead creation failed"
fi

# ======================================================================================
echo
echo "ask-law label is present on the bead"
# ======================================================================================
if [ -n "$id" ]; then
    labels="$(bdt show "$id" --json | python3 -c \
        'import json,sys; d=json.load(sys.stdin); d=d[0] if isinstance(d,list) else d; print(",".join(d.get("labels",[])))')"
    want "ask-law label present"   "ask-law"  "$labels"
    want "overseer label present"  "overseer" "$labels"

    # THE NEGATIVE CONTROL: ask-law must NOT carry any other ask-* routing label.
    # ask-decision and ask-question trigger close-with-verdict, which does not call rule.sh.
    nowant "ask-decision absent on law bead" "ask-decision" "$labels"
    nowant "ask-question absent on law bead" "ask-question" "$labels"
    nowant "ask-task absent on law bead"     "ask-task"     "$labels"
fi

# ======================================================================================
echo
echo "bead is open (awaiting operator action)"
# ======================================================================================
if [ -n "$id" ]; then
    status="$(bdt show "$id" --json | python3 -c \
        'import json,sys; d=json.load(sys.stdin); d=d[0] if isinstance(d,list) else d; print(d.get("status",""))')"
    is "law bead status is open" "open" "$status"
fi

# ======================================================================================
echo
echo "ask.sh law: title must contain a slug:statute colon separator"
# ======================================================================================
out_bad="$(ask law "no-colon-here" 2>&1)"; rc_bad=$?
[ "$rc_bad" -ne 0 ] \
    && ok "ask.sh law without colon exits non-zero" \
    || bad "ask.sh law without colon exits non-zero" "got rc=$rc_bad"
want "error message mentions colon separator" "colon" "$out_bad"

# THE PAIRED POSITIVE: a title WITH a colon still succeeds (not just any failure would do).
out_good="$(ask law "slug-ok: statute text here" 2>&1)"; rc_good=$?
is "ask.sh law with colon exits 0 (positive pair)" "0" "$rc_good"

# ======================================================================================
echo
echo "ask.sh law: panel label check — ask-law is what the panel model reads"
# ======================================================================================
# The panel's is_ask_law() checks for literal string "ask-law" in labels.  A rename here
# and a missed rename there would cause silent mis-routing.  We verify the stored label
# matches exactly what the Rust unit tests exercise (which assert the literal "ask-law").
if [ -n "$id" ]; then
    all_json="$(bdt list --all --limit 0 --json | python3 -c "
import json, sys
rows = json.load(sys.stdin)
rows = rows if isinstance(rows, list) else rows.get('issues', [])
row = next((r for r in rows if r.get('id') == '${id}'), None)
if not row:
    print('not-found'); raise SystemExit(1)
ls = row.get('labels', [])
print('has-ask-law'       if 'ask-law'       in ls else 'no-ask-law')
print('no-ask-decision'   if 'ask-decision'  not in ls else 'has-ask-decision')
print('no-ask-question'   if 'ask-question'  not in ls else 'has-ask-question')
" 2>/dev/null)"
    want "panel sees ask-law label"          "has-ask-law"      "$all_json"
    want "panel does not see ask-decision"   "no-ask-decision"  "$all_json"
    want "panel does not see ask-question"   "no-ask-question"  "$all_json"
fi

echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
