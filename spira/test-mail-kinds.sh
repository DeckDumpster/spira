#!/usr/bin/env bash
# test-mail-kinds.sh — kinds as data: template, per-kind validation, count,
# lint-override header.
#
# Each refusal is tested with a planted offender (SEEN RED) then a passing
# message (SEEN GREEN) so the check's silence is evidence
# (law-absence-needs-a-positive-control).
#
# covers: spira/mail.sh spira/mail/kinds spira/conf.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
isz()    { [ "$2" = 0 ] && ok "$1" || bad "$1" "wanted exit 0 got $2"; }
isnz()   { [ "$2" != 0 ] && ok "$1" || bad "$1" "wanted non-zero exit got 0"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

echo "test-mail-kinds.sh"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM

export SPIRA_MAIL="$TMP/mail"
export SPIRA_MAIL_KINDS="$TMP/kinds"
export SPIRA_CONF=""
export SPIRA_ID_PREFIX="sp"

# Copy the real kinds into a writable fixture directory so we can modify them.
cp -r "$HERE/mail/kinds/." "$TMP/kinds/"

run() { bash "$HERE/mail.sh" "$@"; }

# ==========================================================================
# POSITIVE CONTROL — known kind is accepted, proves checks below are real
# ==========================================================================
echo
echo "positive control — note kind is accepted"

note_body="$(printf '## Note\n\nThis is a note.\n')"
out="$(printf '%s' "$note_body" | run send ctrl --from "Test <t@t>" --subject "A note" --kind note 2>&1)"; rc=$?
isz "SEEN GREEN: note kind accepted" "$rc"

# ==========================================================================
# TEMPLATE — prints the kind's body skeleton
# ==========================================================================
echo
echo "template"

tmpl="$(run template note 2>&1)"; rc=$?
isz  "template note exits 0"    "$rc"
want "template note contains ## Note" "## Note" "$tmpl"

tmpl="$(run template decision 2>&1)"; rc=$?
isz  "template decision exits 0" "$rc"
want "template decision contains ## Decision"            "## Decision"            "$tmpl"
want "template decision contains ## Default"             "## Default"             "$tmpl"
want "template decision contains ## What is blocked"     "## What is blocked"     "$tmpl"
want "template decision contains ## Cost of the wrong choice" "## Cost of the wrong choice" "$tmpl"

out="$(run template unknownkind 2>&1)"; rc=$?
isnz "template unknown kind exits non-zero" "$rc"
want "template unknown kind names the kind" "unknownkind" "$out"

# ==========================================================================
# UNKNOWN KIND (SEEN RED then SEEN GREEN)
# ==========================================================================
echo
echo "unknown kind (SEEN RED then SEEN GREEN)"

out="$(echo "body" | run send mb --from "Sender <s@s>" --subject "Hello" --kind nosuchkind 2>&1)"; rc=$?
isnz "SEEN RED: unknown kind is refused" "$rc"
want "refusal names the rule" "rule" "$out"
want "refusal names the kind" "nosuchkind" "$out"

out="$(printf '## Note\n\nContent.\n' | run send mb --from "Sender <s@s>" --subject "Hello" --kind note 2>&1)"; rc=$?
isz  "SEEN GREEN: known kind is accepted" "$rc"

# ==========================================================================
# MISSING REQUIRED HEADER (SEEN RED then SEEN GREEN)
# decision requires X-Spira-Default (via --default)
# ==========================================================================
echo
echo "missing required header: decision without --default (SEEN RED then SEEN GREEN)"

decision_body="$(printf '## Decision\n\nApprove.\n\n## Default\n\nYes.\n\n## What is blocked\n\nNothing.\n\n## Cost of the wrong choice\n\nLow.\n')"

out="$(printf '%s' "$decision_body" | run send mb --from "Gate <g@g>" --subject "Enable feature?" --kind decision 2>&1)"; rc=$?
isnz "SEEN RED: decision without --default is refused" "$rc"
want "refusal names the rule"    "rule"    "$out"
want "refusal mentions default"  "default" "$out"

out="$(printf '%s' "$decision_body" | run send mb --from "Gate <g@g>" --subject "Enable feature?" --kind decision --default "yes" 2>&1)"; rc=$?
isz  "SEEN GREEN: decision with --default is accepted" "$rc"

# ==========================================================================
# EMPTY REQUIRED SECTION (SEEN RED then SEEN GREEN)
# ==========================================================================
echo
echo "empty required section (SEEN RED then SEEN GREEN)"

# note requires ## Note to be non-empty
out="$(printf '## Note\n\n' | run send mb --from "Sender <s@s>" --subject "Empty note" --kind note 2>&1)"; rc=$?
isnz "SEEN RED: note with empty ## Note section is refused" "$rc"
want "refusal names the rule"    "rule"    "$out"
want "refusal mentions section"  "Note"    "$out"

out="$(printf '## Note\n\nSome content here.\n' | run send mb --from "Sender <s@s>" --subject "Good note" --kind note 2>&1)"; rc=$?
isz  "SEEN GREEN: note with filled ## Note section is accepted" "$rc"

# suit requires Suit, Grounds, and Relief sought — test that a missing middle section fails
suit_missing_grounds="$(printf '## Suit\n\nI claim the service is down.\n\n## Grounds\n\n## Relief sought\n\nFix it.\n')"
out="$(printf '%s' "$suit_missing_grounds" | run send mb --from "Sender <s@s>" --subject "Filing suit" --kind suit 2>&1)"; rc=$?
isnz "SEEN RED: suit with empty ## Grounds section is refused" "$rc"
want "refusal names the rule"      "rule"    "$out"
want "refusal mentions the section" "Grounds" "$out"

suit_full="$(printf '## Suit\n\nI claim the service is down.\n\n## Grounds\n\nThe monitor reported no response for 30 minutes.\n\n## Relief sought\n\nRestart the service.\n')"
out="$(printf '%s' "$suit_full" | run send mb --from "Sender <s@s>" --subject "Filing suit" --kind suit 2>&1)"; rc=$?
isz  "SEEN GREEN: suit with all sections filled is accepted" "$rc"

# ==========================================================================
# URGENT WITHOUT "## Why it is urgent" (SEEN RED then SEEN GREEN)
# ==========================================================================
echo
echo "urgent without ## Why it is urgent (SEEN RED then SEEN GREEN)"

out="$(printf '## Note\n\nContent.\n' | run send mb --from "Sender <s@s>" --subject "Urgent matter" --kind note --urgent 2>&1)"; rc=$?
isnz "SEEN RED: urgent message without ## Why it is urgent is refused" "$rc"
want "refusal names the rule"          "rule"    "$out"
want "refusal mentions urgency"        "urgent"  "$out"

out="$(printf '## Note\n\nContent.\n\n## Why it is urgent\n\nThe system is on fire.\n' | \
    run send mb --from "Sender <s@s>" --subject "Urgent matter" --kind note --urgent 2>&1)"; rc=$?
isz  "SEEN GREEN: urgent message with ## Why it is urgent is accepted" "$rc"

# ==========================================================================
# PROMISED ASK WITH NO ASK (SEEN RED then SEEN GREEN)
# A body that says "the ask below carries the failure" (or similar) but has no
# ## Question or ## Decision section is lying: the operator has nowhere to act.
# ==========================================================================
echo
echo "promised ask with no ask present (SEEN RED then SEEN GREEN)"

out="$(printf '## Note\n\nthe ask below carries the failure\n' | \
    run send mb --from "Sender <s@s>" --subject "An event" --kind note 2>&1)"; rc=$?
isnz "SEEN RED: body promising 'the ask below' with no ## Question section is refused" "$rc"
want "refusal names the rule"    "rule"     "$out"
want "refusal mentions the ask"  "ask"      "$out"

# variant: "the question below"
out="$(printf '## Note\n\nSee the question below for details.\n' | \
    run send mb --from "Sender <s@s>" --subject "An event" --kind note 2>&1)"; rc=$?
isnz "SEEN RED: 'the question below' with no ## Question section is also refused" "$rc"

# SEEN GREEN: body with the promise AND a ## Question section is accepted.
out="$(printf '## Note\n\nthe ask below carries the failure\n\n## Question\n\nDrop the bead?\n' | \
    run send mb --from "Sender <s@s>" --subject "An event" --kind note 2>&1)"; rc=$?
isz  "SEEN GREEN: promised ask fulfilled by a ## Question section is accepted" "$rc"

# SEEN GREEN: a body with no promise phrase is accepted normally.
out="$(printf '## Note\n\nThe bead was poisoned.\n' | \
    run send mb --from "Sender <s@s>" --subject "An event" --kind note 2>&1)"; rc=$?
isz  "SEEN GREEN: note body with no promise phrase is accepted" "$rc"

# ==========================================================================
# EDITING A KIND FILE CHANGES ACCEPTANCE — no code change needed
# ==========================================================================
echo
echo "editing a kind file changes acceptance with no code change"

# Create a custom kind that requires nothing initially.
cat > "$TMP/kinds/custom.md" << 'EOF'
---
---

## Body
EOF

custom_body="$(printf '## Body\n\nHello.\n')"
out="$(printf '%s' "$custom_body" | run send mb --from "Sender <s@s>" --subject "Custom" --kind custom 2>&1)"; rc=$?
isz "SEEN GREEN: custom kind with no required header is accepted" "$rc"

# Now tighten the kind to require X-Spira-Default — no code change, only kind file edit.
cat > "$TMP/kinds/custom.md" << 'EOF'
---
requires: X-Spira-Default
---

## Body
EOF

out="$(printf '%s' "$custom_body" | run send mb --from "Sender <s@s>" --subject "Custom" --kind custom 2>&1)"; rc=$?
isnz "SEEN RED: after tightening kind file, same message is refused" "$rc"
want "refusal names the rule"   "rule"    "$out"
want "refusal mentions default" "default" "$out"

# ==========================================================================
# COUNT — zero, one, many unread messages
# ==========================================================================
echo
echo "count: zero, one, many"

cnt="$(run count empty-box 2>&1)"; rc=$?
isz "count exits 0 on empty mailbox" "$rc"
is  "count is 0 on empty mailbox"    "0" "$cnt"

printf '## Note\n\nFirst.\n' | run send cnt-box --from "A <a@a>" --subject "One" --kind note >/dev/null
cnt="$(run count cnt-box 2>&1)"; rc=$?
isz "count exits 0 with one message" "$rc"
is  "count is 1 with one unread"    "1" "$cnt"

printf '## Note\n\nSecond.\n' | run send cnt-box --from "A <a@a>" --subject "Two"   --kind note >/dev/null
printf '## Note\n\nThird.\n'  | run send cnt-box --from "A <a@a>" --subject "Three" --kind note >/dev/null
cnt="$(run count cnt-box 2>&1)"; rc=$?
isz "count exits 0 with many messages" "$rc"
is  "count is 3 with three unread"     "3" "$cnt"

# Reading one moves it to cur; count should decrease.
run read cnt-box >/dev/null 2>&1
cnt="$(run count cnt-box 2>&1)"
is "count decreases after reading one message" "2" "$cnt"

# ==========================================================================
# LINT OVERRIDE — SPIRA_MAIL_LINT_CONSIDERED=<reason> recorded in header
# ==========================================================================
echo
echo "lint override: reason recorded in X-Spira-Lint-Override"

# A message that would fail lint (missing From and Subject) but override is set.
msg_file="$TMP/mail/override-box/new"
mkdir -p "$msg_file"

out="$(echo "body" | SPIRA_MAIL_LINT_CONSIDERED="testing override" bash "$HERE/mail.sh" send override-box 2>&1)"; rc=$?
isz "SEEN GREEN: override bypasses missing From and Subject" "$rc"

# Find the delivered message and check for the header.
msg="$(ls "$SPIRA_MAIL/override-box/new/" | head -1)"
msg_content="$(cat "$SPIRA_MAIL/override-box/new/$msg")"
want "X-Spira-Lint-Override header is present"   "X-Spira-Lint-Override" "$msg_content"
want "X-Spira-Lint-Override records the reason"  "testing override"      "$msg_content"

echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
