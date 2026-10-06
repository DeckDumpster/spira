#!/usr/bin/env bash
#
# test-rebase-escalation.sh — spira_ask_rebase_loop tells the Concierge, never Ryan
# (law-a-rebase-loop-is-sequenced-not-split): a machine event to the concierge mailbox
# naming the conflicting files, whether each is GENERATED, and the beads that changed them
# on the base — never a --kind question/decision, which is what files a needs-ryan bead.
#
# ACCEPTANCE CRITERIA (from sp-qql20):
#   1. the event lands in the concierge mailbox, not the operator's
#   2. no needs-ryan (decision) bead is filed
#   3. conflicting files are named, including a non-generated one (positive control)
#   4. a GENERATED file (declared in SPIRA_REBASE_GENERATED_FILES) is flagged: regenerate,
#      don't merge — a non-generated file alongside it must NOT carry that flag
#   5. beads that changed the conflicted files on the base are named
#   6. closed bead status still appears in the body
#   7. no blank slot in the suggested action when others is empty
#      (law-absence-needs-a-positive-control)
#
# REGRESSION (law-a-regression-test-must-be-seen-to-fail): against the code before this
# change, spira_ask_rebase_loop sends --kind question to the operator mailbox, which makes
# mail file a decision bead labeled needs-operator. Cases 1 and 2 below both fail on that
# code: the event lands in operator/ instead of concierge/, and a needs-operator bead
# appears where zero are expected. Verified by hand against the pre-change lib.sh.
#
# A REAL mail AND A REAL bd ON A FIXTURE DATABASE — not a stub, so the needs-ryan-bead
# assertion exercises the actual mechanism that would file one (mail's own kind==question
# tracking-bead logic), rather than a hand-written model of it.
#
# sp-31hjr: spira_ask_rebase_loop is native in landing-pass now (was lib.sh) — driven
# through `landing-pass ask-rebase-loop <id> <branch> <repo> <n> <conflicts> <others>`
# against the real compiled binary, same real-sender contract as before.
#
# tier: T2
# covers: landing-pass/src/* mail/src/* spira/lib.sh
# hermetic-ok: uses a fixture database and a fixture SPIRA_MAIL dir, no systemd or gh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

command -v landing-pass >/dev/null 2>&1 || bail "landing-pass is not on PATH"

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-rebase-escalation
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up rebase-escalation || { echo "test-rebase-escalation: could not build a fixture database"; exit 1; }

RUN="$TMP/run"; mkdir -p "$RUN"
export SPIRA_HOME="$HERE"
export SPIRA_MAIL_REPEAT_CONSIDERED="test-suite"
# SPIRA_CONCIERGE_INBOX EXPLICITLY: landing-pass's mail send to "concierge" runs
# inbox-append.sh (per SPIRA_MAIL_READERS's default), which resolves SPIRA_CONCIERGE_INBOX
# from config — the complete fixture's own value is a fixed, unwritable "/fixture/home/..."
# path now, hence "mkdir /fixture: Permission denied" and an empty mail body downstream.
tl_config SPIRA_RUN="$RUN" SPIRA_MAIL="$TMP/mail" SPIRA_MAIL_KINDS="$HERE/mail/kinds" \
    SPIRA_ASK_LABEL="needs-operator" SPIRA_ID_PREFIX="sp" SPIRA_MAIL_MUTE=0 \
    SPIRA_CONCIERGE_INBOX="$TMP/concierge-inbox.log"

ask_rebase_loop() {   # ask_rebase_loop <id> <branch> <repo> <n> <conflicts> <others>
    landing-pass ask-rebase-loop "$1" "$2" "$3" "$4" "$5" "$6"
}

body_of() {
    local mailbox="$1" f
    f="$(ls -t "$SPIRA_MAIL/$mailbox/new"/* 2>/dev/null | head -1)"
    [ -n "$f" ] && sed '1,/^$/d' "$f"
}

count_new() { ls "$SPIRA_MAIL/$1/new" 2>/dev/null | wc -l | tr -d ' '; }

needs_ryan_count() {
    "${SPIRA_BD:-bd}" -C "$SPIRA_DB" list --status open --label "${SPIRA_ASK_LABEL}" --limit 0 --json 2>/dev/null \
        | sed -n '/^[[{]/,$p' \
        | python3 -c '
import sys, json
try: d = json.load(sys.stdin)
except Exception: print(0); sys.exit()
rows = d if isinstance(d, list) else [d]
print(len(rows))' 2>/dev/null
}

reset_mail() { rm -rf "$SPIRA_MAIL"; mkdir -p "$SPIRA_MAIL"; }

echo "test-rebase-escalation.sh"

# ======================================================================================
echo
echo "event reaches the Concierge, never the operator, and files no needs-ryan bead:"
# ======================================================================================
testdb_reset
printf '{"id":"sp-t001","title":"express lane: exempts critical beads","status":"in_progress","issue_type":"task","labels":["plan"],"updated_at":"2026-09-01T00:00:00Z"}\n' \
    | testdb_seed
reset_mail
before="$(needs_ryan_count)"
ask_rebase_loop "sp-t001" "spira/sp-t001" "spira" "3" "foo.sh" ""
after="$(needs_ryan_count)"

is   "event lands in the concierge mailbox" 1 "$(count_new concierge)"
is   "nothing lands in the operator mailbox" 0 "$(count_new operator)"
is   "no needs-ryan bead is filed" "$before" "$after"

body="$(body_of concierge)"
want "subject is the rebase-loop event" "rebase loop" "$body"

# ======================================================================================
echo
echo "empty others — suggested action has no blank slot:"
# ======================================================================================
# POSITIVE CONTROL: a subject that would have had a blank slot in the old code must
# not have one in the new code (law-absence-needs-a-positive-control proved below).
body="$(body_of concierge)"
nowant "no 'duplicate of' clause" "is a duplicate of" "$body"
want   "suggested action says rebase by hand" "by hand and push" "$body"

# ======================================================================================
echo
echo "conflicting files are named — non-generated file carries no GENERATED flag:"
# ======================================================================================
want    "names the conflicting file" "foo.sh" "$body"
nowant  "non-generated file is not flagged GENERATED" "foo.sh (GENERATED" "$body"

# ======================================================================================
echo
echo "a GENERATED file is flagged: regenerate, do not merge:"
# ======================================================================================
testdb_reset
printf '{"id":"sp-t002g","title":"some open task","status":"in_progress","issue_type":"task","labels":["plan"],"updated_at":"2026-09-01T00:00:00Z"}\n' \
    | testdb_seed
reset_mail
ask_rebase_loop "sp-t002g" "spira/sp-t002g" "spira" "3" "docs/test-plan/coverage.json bar.sh" ""
body="$(body_of concierge)"
want   "GENERATED file is named" "docs/test-plan/coverage.json" "$body"
want   "GENERATED file is flagged to regenerate, not merge" "docs/test-plan/coverage.json (GENERATED — regenerate it, do not merge it by hand)" "$body"
nowant "the ordinary conflicted file alongside it is not flagged" "bar.sh (GENERATED" "$body"
want   "suggested action points at regenerating" "regenerate the GENERATED file" "$body"

# ======================================================================================
echo
echo "positive control — non-empty others names those beads:"
# ======================================================================================
testdb_reset
printf '{"id":"sp-t002","title":"some open task","status":"in_progress","issue_type":"task","labels":["plan"],"updated_at":"2026-09-01T00:00:00Z"}\n' \
    | testdb_seed
reset_mail
ask_rebase_loop "sp-t002" "spira/sp-t002" "spira" "4" "bar.sh" "sp-other1 sp-other2"

body="$(body_of concierge)"
want "names first other bead"  "sp-other1" "$body"
want "names second other bead" "sp-other2" "$body"
want "suggested action includes duplicate clause" "is a duplicate of" "$body"

# ======================================================================================
echo
echo "closed bead — status appears in body:"
# ======================================================================================
testdb_reset
printf '{"id":"sp-t003","title":"a closed task","status":"closed","issue_type":"task","labels":["plan"],"updated_at":"2026-09-01T00:00:00Z"}\n' \
    | testdb_seed
reset_mail
ask_rebase_loop "sp-t003" "spira/sp-t003" "spira" "5" "baz.sh" ""

body="$(body_of concierge)"
want "closed status in body" "Status: closed" "$body"

tl_summary
