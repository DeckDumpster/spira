#!/usr/bin/env bash
#
# test-mail-tidy.sh — mail.sh tidy: archive old, answered and closed operator mail.
#
# mail.sh tidy <mailbox> moves messages to archive/cur unless they should be kept.
# A message is KEPT only if one of these holds:
#   1. Its bead ID refers to an open, ask-labelled bead.
#   2. It is unread and younger than SPIRA_MAIL_TIDY_FRESH.
#   3. It carries X-Spira-Urgent and is younger than 7 days (urgent_max_s = 604800).
# Older copies of a repeated subject are always archived (dedup, newest only).
# When the bead store is unreadable, nothing moves.
#
# POSITIVE CONTROL: the fixture has an open-ask message. Tidy is confirmed to archive the
# closed-ask message (SEEN RED), then the open-ask message is verified still present. This
# ensures tidy ran and the keep rule is what saved it, not inaction
# (law-absence-needs-a-positive-control).
#
# SPIRA_ASK_LABEL is pinned to a non-default value so a hardcoded literal in the tidy
# parser would fail to match the fixture (law-gates-run-in-a-clean-environment).
#
# The bead store is a stub `bd` answering the one query cmd_tidy makes (open ask-labelled
# ids) rather than an embedded-Dolt database: the open-ask list is one command with one
# output, and a real database bought nothing this suite used except several seconds of
# startup (test-plan-2026-09-23 §3 row 08, coverage-map DEMOTE-TO-T2).
#
# tier: T2
# covers: spira/mail.sh spira/conf.sh UC-operator-channel-08
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM

export SPIRA_MAIL="$TMP/mail"
export SPIRA_CONF=/nonexistent
export SPIRA_ID_PREFIX="sp"
export SPIRA_ASK_LABEL=asks-test    # non-default: catches hardcoded literals
export SPIRA_MAIL_TIDY_FRESH=3600   # non-default: 1h window
export SPIRA_DB="$TMP/db"

MAIL="$HERE/mail.sh"
OLD_AGE=7200    # older than SPIRA_MAIL_TIDY_FRESH
URGENT_MAX_S=604800

# The stub answers `list --status ... --label $SPIRA_ASK_LABEL --limit 0 --brief --json`
# with the ids in $OPEN_IDS_FILE, and the positive-control probe (`list --limit 1 ...`)
# with a single row — both queries cmd_tidy makes, nothing else. STUB_BD_FAIL simulates an
# unreadable store for the guard case.
STUB_BD="$TMP/bd-stub.sh"
OPEN_IDS_FILE="$TMP/open-ids.json"
printf '[]\n' > "$OPEN_IDS_FILE"
cat > "$STUB_BD" <<STUB
#!/usr/bin/env bash
[ -n "\${STUB_BD_FAIL:-}" ] && { echo "bd: stub failure" >&2; exit 1; }
label=""
prev=""
for a in "\$@"; do
    [ "\$prev" = "--label" ] && label="\$a"
    prev="\$a"
done
if [ "\$label" = "$SPIRA_ASK_LABEL" ]; then
    cat "$OPEN_IDS_FILE"
else
    printf '[{"id":"probe"}]\n'
fi
STUB
chmod +x "$STUB_BD"
export SPIRA_BD="$STUB_BD"

run_tidy() { bash "$MAIL" tidy operator "$@"; }

send_msg() {   # send_msg <mailbox> <subject> <bead-id|-> [--urgent]
    local mbox="$1" subj="$2" bid="$3"; shift 3
    local extra=()
    [ "$bid" != "-" ] && extra+=(--bead "$bid")
    [ "${1:-}" = "--urgent" ] && extra+=(--urgent)
    echo "body" | SPIRA_MAIL_LINT_CONSIDERED="test" SPIRA_MAIL_REPEAT_CONSIDERED="fixture" \
        bash "$MAIL" send "$mbox" \
            --from "Bot <bot@spira>" \
            --subject "$subj" \
            "${extra[@]}" 2>/dev/null
}

# Diffs new/ before and after so the exact file is tracked even when two sends share a
# second (avoids ls -t mtime collisions).
send_track() {
    local mailbox="$1" subject="$2" bid="$3"; shift 3
    local before_s after_s new_file
    before_s="$(ls "$SPIRA_MAIL/$mailbox/new/" 2>/dev/null | sort)"
    send_msg "$mailbox" "$subject" "$bid" "$@"
    after_s="$(ls "$SPIRA_MAIL/$mailbox/new/" 2>/dev/null | sort)"
    new_file="$(comm -13 <(printf '%s\n' "$before_s") <(printf '%s\n' "$after_s") | head -1)"
    printf '%s/%s/new/%s' "$SPIRA_MAIL" "$mailbox" "$new_file"
}

backdate() { touch -d "@$(( $(date +%s) - $2 ))" "$1"; }   # backdate <file> <age_seconds>

mark_read() {   # mark_read <path-in-new> -> moves to cur with S flag; prints new path
    local f="$1" base dst
    base="$(basename "$f")"
    dst="$(dirname "$f")/../cur/${base}:2,S"
    mv "$f" "$dst"
    printf '%s' "$dst"
}

echo
echo "=== Setup: populate operator inbox ==="

mkdir -p "$SPIRA_MAIL/operator/new" "$SPIRA_MAIL/operator/cur" "$SPIRA_MAIL/operator/tmp"

OPEN_ID="sp-open1"
printf '[{"id":"%s"}]\n' "$OPEN_ID" > "$OPEN_IDS_FILE"

# Message A: open-ask question — should be KEPT (rule 1)
MSG_A="$(send_track operator "Open question" "$OPEN_ID")"
ok "message A (open-ask) created: $(basename "$MSG_A")"

# Message B: closed-ask question, old — should be ARCHIVED (bead not in the open-ids list)
MSG_B="$(send_track operator "Closed question" "sp-closed1")"
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

total_before=$(( $(ls "$SPIRA_MAIL/operator/new/" 2>/dev/null | wc -l) \
              + $(ls "$SPIRA_MAIL/operator/cur/" 2>/dev/null | wc -l) ))
is "SEEN RED: inbox has 7 messages before tidy" "7" "$total_before"

echo
echo "=== POSITIVE CONTROL: closed-ask is archived; open-ask stays ==="

out="$(run_tidy 2>&1)"
is "tidy exits 0" "0" "$?"
want "tidy reports archived count" "archived" "$out"
want "tidy reports kept count"     "kept"     "$out"

exists() { [ -f "$1" ] && echo 1 || echo 0; }

is "SEEN RED (tidy ran): closed-ask archived" "0" "$(exists "$MSG_B")"
is "open-ask message still in operator inbox" "1" "$(exists "$MSG_A")"

echo
echo "=== Verify: expected messages kept and archived ==="

inbox_after=$(( $(ls "$SPIRA_MAIL/operator/new/" 2>/dev/null | wc -l) \
              + $(ls "$SPIRA_MAIL/operator/cur/" 2>/dev/null | wc -l) ))
is "inbox has 3 messages after tidy (A+D+E3)" "3" "$inbox_after"

archive_after=$(ls "$SPIRA_MAIL/archive/cur/" 2>/dev/null | wc -l | tr -d ' ')
is "archive has 4 messages after tidy (B+C+E1+E2)" "4" "$archive_after"

is "A (open-ask) kept"          "1" "$(exists "$MSG_A")"
is "C (read, old) archived"     "0" "$(exists "$MSG_C")"
is "D (fresh, unread) kept"     "1" "$(exists "$MSG_D")"
is "E1 (oldest daily) archived" "0" "$(exists "$MSG_E1")"
is "E2 (middle daily) archived" "0" "$(exists "$MSG_E2")"
is "E3 (newest daily) kept"     "1" "$(exists "$MSG_E3")"

echo
echo "=== Gap G-02: urgent retention at both edges of the 7-day window ==="

MSG_URGENT_IN="$(send_track operator "Urgent still fresh" - --urgent)"
backdate "$MSG_URGENT_IN" $(( URGENT_MAX_S - 60 ))
MSG_URGENT_OUT="$(send_track operator "Urgent expired" - --urgent)"
backdate "$MSG_URGENT_OUT" $(( URGENT_MAX_S + 60 ))
# Both are read (old, unread would already be kept by rule 2 for the in-window one at this
# age — mark read so only rule 3, the urgent rule, can be the reason either is kept).
mark_read "$MSG_URGENT_IN" >/dev/null
mark_read "$MSG_URGENT_OUT" >/dev/null
MSG_URGENT_IN="$SPIRA_MAIL/operator/cur/$(basename "$MSG_URGENT_IN"):2,S"
MSG_URGENT_OUT="$SPIRA_MAIL/operator/cur/$(basename "$MSG_URGENT_OUT"):2,S"
backdate "$MSG_URGENT_IN" $(( URGENT_MAX_S - 60 ))
backdate "$MSG_URGENT_OUT" $(( URGENT_MAX_S + 60 ))

run_tidy >/dev/null 2>&1
is "G-02: urgent message 60s inside the 7-day window is kept" "1" "$(exists "$MSG_URGENT_IN")"
is "G-02: urgent message 60s past the 7-day window is archived" "0" "$(exists "$MSG_URGENT_OUT")"

echo
echo "=== Guard: bead store unreadable — nothing moves ==="

send_msg operator "Guard test" "$OPEN_ID"
inbox_before_guard=$(( $(ls "$SPIRA_MAIL/operator/new/" 2>/dev/null | wc -l) \
                      + $(ls "$SPIRA_MAIL/operator/cur/" 2>/dev/null | wc -l) ))

rc=0
out="$(STUB_BD_FAIL=1 run_tidy 2>&1)" || rc=$?
[ "$rc" -ne 0 ] && ok "tidy refuses when bead store unreadable (non-zero exit)" \
                || bad "tidy refuses when bead store unreadable" "expected non-zero exit, got 0"

inbox_after_guard=$(( $(ls "$SPIRA_MAIL/operator/new/" 2>/dev/null | wc -l) \
                     + $(ls "$SPIRA_MAIL/operator/cur/" 2>/dev/null | wc -l) ))
is "nothing moved when bead store unreadable" "$inbox_before_guard" "$inbox_after_guard"
want "refusal message mentions refusing" "refusing" "$out"

echo
echo "=== Dry run: reports counts without moving ==="

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

echo
echo "=== Fresh install: a mailbox install created but no mail ever reached ==="
# THE DEFECT. The operator mailbox was created lazily, by the first mail sent to it, so on a
# never-used install it did not exist and the first tidy (its timer fires the moment it is
# enabled on a box up longer than OnBootSec) exited 1 — "tidy: operator: mailbox not found"
# — and left spira-mail-tidy FAILED, which deploy's pre-health check refuses on. install.sh
# now creates it with `mail.sh ensure operator`; tidy's own refusal of a mailbox that does
# not exist stays (a misconfigured SPIRA_MAIL must not tidy silently).
FRESH="$TMP/fresh-mail"
out="$(SPIRA_MAIL="$FRESH" bash "$MAIL" tidy operator 2>&1)"; rc=$?
[ "$rc" -ne 0 ] && ok "positive control: tidy of a mailbox nothing created still refuses" \
                || bad "positive control: tidy of a mailbox nothing created still refuses" "rc=0"
want "positive control: and says why" "mailbox not found" "$out"
SPIRA_MAIL="$FRESH" bash "$MAIL" ensure operator; rc=$?
is "mail.sh ensure operator exits 0" 0 "$rc"
[ -d "$FRESH/operator/new" ] && [ -d "$FRESH/operator/cur" ] && [ -d "$FRESH/operator/tmp" ] \
    && ok  "ensure creates the operator maildir (new, cur, tmp)" \
    || bad "ensure creates the operator maildir (new, cur, tmp)" "$(ls -R "$FRESH" 2>&1 | head -5)"
out="$(SPIRA_MAIL="$FRESH" bash "$MAIL" tidy operator 2>&1)"; rc=$?
is     "tidy of the ensured, empty mailbox exits 0" 0 "$rc"
nowant "and does not report it missing" "mailbox not found" "$out"
want   "install.sh ensures the operator mailbox" 'mail.sh" ensure operator' "$(cat "$HERE/../install.sh")"

tl_summary
