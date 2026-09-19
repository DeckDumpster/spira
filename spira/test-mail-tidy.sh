#!/usr/bin/env bash
#
# test-mail-tidy.sh — mail.sh tidy: archive old, answered and closed operator mail.
#
# WHAT THIS TESTS
# ---------------
# mail.sh tidy <mailbox> moves messages to archive/cur unless they should be kept.
# A message is KEPT only if one of these holds:
#   1. Its bead ID refers to an open, ask-labelled bead.
#   2. It is unread and younger than SPIRA_MAIL_TIDY_FRESH.
#   3. It carries X-Spira-Urgent and is younger than 7 days.
# Older copies of a repeated subject are always archived (dedup, newest only).
# When the bead store is unreadable, nothing moves.
#
# POSITIVE CONTROL: The fixture has an open-ask message. Tidy is confirmed to
# archive the closed-ask message (SEEN RED), then the open-ask message is verified
# still present. This ensures tidy ran and the keep rule is what saved it, not
# inaction (law-absence-needs-a-positive-control).
#
# SPIRA_ASK_LABEL is pinned to a non-default value so a hardcoded literal in the
# tidy parser would fail to match the fixture (law-gates-run-in-a-clean-environment).
#
# FILE TRACKING: send_track() diffs new/ before and after each send so the exact
# file is tracked even when multiple messages share a mtime second.
#
# covers: spira/mail.sh spira/conf.sh systemd/spira-mail-tidy.service systemd/spira-mail-tidy.timer systemd/units.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
UNIT_DIR="$HERE/../systemd"

pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "${2:-}"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "expected [$2] got [$3]"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }

# 1 = file exists, 0 = gone
exists() { [ -f "$1" ] && echo 1 || echo 0; }

echo "test-mail-tidy.sh"

. "$HERE/testdb.sh"
testdb_require test-mail-tidy
testdb_up mailtidy || { echo "testdb_up failed"; exit 1; }

TMP="$(mktemp -d)"
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM

export SPIRA_MAIL="$TMP/mail"
export SPIRA_CONF=/nonexistent
export SPIRA_ID_PREFIX="sp"
export SPIRA_ASK_LABEL=asks-test    # non-default: catches hardcoded literals
export SPIRA_MAIL_TIDY_FRESH=3600   # non-default: 1h window

MAIL="$HERE/mail.sh"
OLD_AGE=7200    # older than SPIRA_MAIL_TIDY_FRESH

run_tidy() { bash "$MAIL" tidy operator "$@"; }

send_msg() {   # send_msg <mailbox> <subject> <bead-id|->
    local mbox="$1" subj="$2" bid="$3"
    local extra=""
    [ "$bid" != "-" ] && extra="--bead $bid"
    # shellcheck disable=SC2086
    echo "body" | SPIRA_MAIL_LINT_CONSIDERED="test" SPIRA_MAIL_REPEAT_CONSIDERED="fixture" \
        bash "$MAIL" send "$mbox" \
            --from "Bot <bot@spira>" \
            --subject "$subj" \
            $extra 2>/dev/null
}

# Returns the full path of the file just sent. Diffs new/ before and after to
# find the exact file, avoiding ls -t mtime collisions when sends happen rapidly.
send_track() {
    local mailbox="$1" subject="$2" bid="$3"
    local before_s after_s new_file
    before_s="$(ls "$SPIRA_MAIL/$mailbox/new/" 2>/dev/null | sort)"
    send_msg "$mailbox" "$subject" "$bid"
    after_s="$(ls "$SPIRA_MAIL/$mailbox/new/" 2>/dev/null | sort)"
    new_file="$(comm -13 <(printf '%s\n' "$before_s") <(printf '%s\n' "$after_s") | head -1)"
    printf '%s/%s/new/%s' "$SPIRA_MAIL" "$mailbox" "$new_file"
}

backdate() {   # backdate <file> <age_seconds>
    touch -d "@$(( $(date +%s) - $2 ))" "$1"
}

mark_read() {   # mark_read <path-in-new> → moves to cur with S flag; prints new path
    local f="$1" base
    base="$(basename "$f")"
    local dst; dst="$(dirname "$f")/../cur/${base}:2,S"
    mv "$f" "$dst"
    printf '%s' "$dst"
}

# =============================================================================
echo
echo "=== Setup: seed bead database ==="
# =============================================================================

ASK="$SPIRA_ASK_LABEL"

OPEN_ID="$(bd -C "$SPIRA_DB" create "open ask" -l "$ASK,overseer" \
    --type decision --silent 2>/dev/null)" || OPEN_ID=""
if [ -z "$OPEN_ID" ]; then
    bad "seed open-ask bead" "bd create returned empty id"
    printf '\n%d passed, %d FAILED\n' "$pass" "$fail"; exit 1
fi
ok "open-ask bead created: $OPEN_ID"

CLOSED_ID="$(bd -C "$SPIRA_DB" create "closed ask" -l "$ASK,overseer" \
    --type decision --silent 2>/dev/null)" || CLOSED_ID=""
[ -n "$CLOSED_ID" ] && bd -C "$SPIRA_DB" close "$CLOSED_ID" --reason-file - <<< "done" >/dev/null 2>&1
if [ -z "$CLOSED_ID" ]; then
    bad "seed closed-ask bead" "bd create returned empty id"
    printf '\n%d passed, %d FAILED\n' "$pass" "$fail"; exit 1
fi
ok "closed-ask bead created and closed: $CLOSED_ID"

# =============================================================================
echo
echo "=== Setup: populate operator inbox ==="
# =============================================================================

mkdir -p "$SPIRA_MAIL/operator/new" "$SPIRA_MAIL/operator/cur" "$SPIRA_MAIL/operator/tmp"

# Message A: open-ask question — should be KEPT (rule 1)
MSG_A="$(send_track operator "Open question" "$OPEN_ID")"
ok "message A (open-ask) created: $(basename "$MSG_A")"

# Message B: closed-ask question, old — should be ARCHIVED (closed bead, old)
MSG_B="$(send_track operator "Closed question" "$CLOSED_ID")"
backdate "$MSG_B" "$OLD_AGE"
ok "message B (closed-ask, old) created"

# Message C: read note, old — should be ARCHIVED (read, old)
MSG_C_NEW="$(send_track operator "Read note" -)"
MSG_C="$(mark_read "$MSG_C_NEW")"
backdate "$MSG_C" "$OLD_AGE"
ok "message C (read, old) created: $(basename "$MSG_C")"

# Message D: fresh unread note — should be KEPT (rule 2)
MSG_D="$(send_track operator "Fresh note" -)"
ok "message D (fresh, unread) created"

# Messages E1, E2, E3: three copies of same subject — only newest (E3) KEPT
MSG_E1="$(send_track operator "Daily report" -)"
backdate "$MSG_E1" $(( OLD_AGE * 2 ))
MSG_E2="$(send_track operator "Daily report" -)"
backdate "$MSG_E2" "$OLD_AGE"
MSG_E3="$(send_track operator "Daily report" -)"
ok "messages E1/E2/E3 (three copies of 'Daily report') created"

# Verify inbox has 7 messages before tidy (SEEN RED — confirms fixture is real)
total_before=$(( $(ls "$SPIRA_MAIL/operator/new/" 2>/dev/null | wc -l) \
              + $(ls "$SPIRA_MAIL/operator/cur/" 2>/dev/null | wc -l) ))
is "SEEN RED: inbox has 7 messages before tidy" "7" "$total_before"

# =============================================================================
echo
echo "=== POSITIVE CONTROL: closed-ask is archived; open-ask stays ==="
# =============================================================================

out="$(run_tidy 2>&1)"
rc=$?
is "tidy exits 0" "0" "$rc"
want "tidy reports archived count" "archived" "$out"
want "tidy reports kept count"     "kept"     "$out"

# POSITIVE CONTROL: tidy ran and archived the closed-ask message (SEEN RED).
# If this passes, silence on the open-ask is meaningful — not just inaction.
is "SEEN RED (tidy ran): closed-ask archived" "0" "$(exists "$MSG_B")"

is "open-ask message still in operator inbox" "1" "$(exists "$MSG_A")"

# =============================================================================
echo
echo "=== Verify: expected messages kept and archived ==="
# =============================================================================

inbox_after=$(( $(ls "$SPIRA_MAIL/operator/new/" 2>/dev/null | wc -l) \
              + $(ls "$SPIRA_MAIL/operator/cur/" 2>/dev/null | wc -l) ))
is "inbox has 3 messages after tidy (A+D+E3)" "3" "$inbox_after"

archive_after=$(ls "$SPIRA_MAIL/archive/cur/" 2>/dev/null | wc -l | tr -d ' ')
is "archive has 4 messages after tidy (B+C+E1+E2)" "4" "$archive_after"

is "A (open-ask) kept"       "1" "$(exists "$MSG_A")"
is "C (read, old) archived"  "0" "$(exists "$MSG_C")"
is "D (fresh, unread) kept"  "1" "$(exists "$MSG_D")"
is "E1 (oldest daily) archived" "0" "$(exists "$MSG_E1")"
is "E2 (middle daily) archived" "0" "$(exists "$MSG_E2")"
is "E3 (newest daily) kept"     "1" "$(exists "$MSG_E3")"

# =============================================================================
echo
echo "=== Guard: bead store unreadable — nothing moves ==="
# =============================================================================

send_msg operator "Guard test" "$OPEN_ID"
inbox_before_guard=$(( $(ls "$SPIRA_MAIL/operator/new/" 2>/dev/null | wc -l) \
                      + $(ls "$SPIRA_MAIL/operator/cur/" 2>/dev/null | wc -l) ))

rc=0
out="$(SPIRA_DB=/nonexistent/beads run_tidy 2>&1)" || rc=$?
[ "$rc" -ne 0 ] && ok "tidy refuses when bead store unreadable (non-zero exit)" \
                || bad "tidy refuses when bead store unreadable" "expected non-zero exit, got 0"

inbox_after_guard=$(( $(ls "$SPIRA_MAIL/operator/new/" 2>/dev/null | wc -l) \
                     + $(ls "$SPIRA_MAIL/operator/cur/" 2>/dev/null | wc -l) ))
is "nothing moved when bead store unreadable" "$inbox_before_guard" "$inbox_after_guard"
want "refusal message mentions refusing" "refusing" "$out"

# =============================================================================
echo
echo "=== Dry run: reports counts without moving ==="
# =============================================================================

MSG_DRY="$(send_track operator "Dry run test" -)"
backdate "$MSG_DRY" "$OLD_AGE"

inbox_before_dry=$(( $(ls "$SPIRA_MAIL/operator/new/" 2>/dev/null | wc -l) \
                   + $(ls "$SPIRA_MAIL/operator/cur/" 2>/dev/null | wc -l) ))

out="$(run_tidy --dry-run 2>&1)"
is "dry-run exits 0" "0" "$?"
want "dry-run reports archived" "archived" "$out"

inbox_after_dry=$(( $(ls "$SPIRA_MAIL/operator/new/" 2>/dev/null | wc -l) \
                  + $(ls "$SPIRA_MAIL/operator/cur/" 2>/dev/null | wc -l) ))
is "dry-run moves nothing" "$inbox_before_dry" "$inbox_after_dry"

# =============================================================================
echo
echo "=== Service file: ExecStart invokes mail.sh tidy ==="
# =============================================================================

SVC="$UNIT_DIR/spira-mail-tidy.service"
[ -r "$SVC" ] || { bad "spira-mail-tidy.service readable" "not found at $SVC"; }

execstart="$(grep '^ExecStart=' "$SVC" 2>/dev/null | head -1)"
if [ -z "$execstart" ]; then
    bad "spira-mail-tidy.service has ExecStart" "absent"
else
    ok "spira-mail-tidy.service has ExecStart"
    want "ExecStart calls mail.sh" "mail.sh" "$execstart"
    want "ExecStart calls tidy subcommand" "tidy" "$execstart"
    want "ExecStart targets operator mailbox" "operator" "$execstart"
fi

# =============================================================================
echo
echo "=== Timer file: fires every 15 minutes ==="
# =============================================================================

TMR="$UNIT_DIR/spira-mail-tidy.timer"
[ -r "$TMR" ] || { bad "spira-mail-tidy.timer readable" "not found at $TMR"; }

onactive="$(grep '^OnUnitActiveSec=' "$TMR" 2>/dev/null | head -1)"
if [ -z "$onactive" ]; then
    bad "spira-mail-tidy.timer has OnUnitActiveSec" "absent — fires once per boot only"
else
    ok "spira-mail-tidy.timer has OnUnitActiveSec"
    want "fires every 15min" "15min" "$onactive"
fi

unit_line="$(grep '^Unit=' "$TMR" 2>/dev/null | head -1 | cut -d= -f2-)"
want "timer names the tidy service" "spira-mail-tidy" "${unit_line:-}"

# =============================================================================
echo
echo "=== units.sh: timer in UNITS and _ENABLE_TMPL ==="
# =============================================================================

UNITS_SH="$UNIT_DIR/units.sh"
units_block="$(awk '/^UNITS=\(/{found=1} found{print} found && /\)/{found=0}' "$UNITS_SH")"
enable_block="$(awk '/_ENABLE_TMPL=\(/{found=1} found{print} found && /\)/{found=0}' "$UNITS_SH")"

# POSITIVE CONTROL: a known entry (spira-sentinel.timer) appears in both blocks.
case "$units_block" in
    *spira-sentinel.timer*) ok "positive control: units_block is parseable" ;;
    *) bad "positive control: units_block is parseable" "spira-sentinel.timer absent — parser broken" ;;
esac
case "$enable_block" in
    *spira-sentinel.timer*) ok "positive control: enable_block is parseable" ;;
    *) bad "positive control: enable_block is parseable" "spira-sentinel.timer absent — parser broken" ;;
esac

case "$units_block" in
    *spira-mail-tidy.timer*)   ok "spira-mail-tidy.timer is in UNITS" ;;
    *) bad "spira-mail-tidy.timer is in UNITS" "absent — install.sh will not write it" ;;
esac
case "$units_block" in
    *spira-mail-tidy.service*) ok "spira-mail-tidy.service is in UNITS" ;;
    *) bad "spira-mail-tidy.service is in UNITS" "absent — install.sh will not write it" ;;
esac
case "$enable_block" in
    *spira-mail-tidy.timer*)   ok "spira-mail-tidy.timer is in _ENABLE_TMPL" ;;
    *) bad "spira-mail-tidy.timer is in _ENABLE_TMPL" "absent — timer will not be enabled" ;;
esac

# =============================================================================
echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
