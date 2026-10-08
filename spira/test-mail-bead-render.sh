#!/usr/bin/env bash
# test-mail-bead-render.sh — mail renders a bead block for every bead it is
# given, so a message that names a bead cannot lack its details
# (law-a-bead-reference-carries-its-details).
#
# Three ways a bead reaches the renderer, and what happens when the store can't
# resolve it:
#   1. --bead <id>            — resolved and rendered, above the producer's prose.
#   2. --bead <id the store cannot resolve> — renders "unresolved: <id>"; send still succeeds.
#   3. an id named only in subject/body, no --bead — renders nothing; a mention is not a
#      citation, and the first id in free text may be a stranger's.
#
# POSITIVE CONTROL (law-absence-needs-a-positive-control): sp-titl01 is seeded with
# known title/status/priority; only those seeded values can make case 1's assertions
# pass, so a renderer that always prints "unresolved" cannot pass case 1 while also
# passing case 3's "unresolved" assertion — the two cases together prove the lookup
# is real.
#
# REGRESSION (law-a-regression-test-must-be-seen-to-fail): the last case sends the
# exact subject shape lib.sh's gh-closeout path produces — "Close GitHub issue ... for
# bead <id>", carrying nothing else — the case that motivated this bead: a producer
# that names a bead and says nothing about it. Against mail before this change, no
# render step exists at all, so this assertion fails; verified by hand against the
# pre-change mail.
#
# tier: T2
# covers: mail/src/* spira/lib.sh spira/conf.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

echo "test-mail-bead-render.sh"

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-mail-bead-render
TMP="$(mktemp -d)"
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up mail-bead-render || { echo "test-mail-bead-render: could not build fixture database"; exit 1; }

SPIRA_MAIL="$TMP/mail"
SPIRA_MAIL_KINDS="$HERE/mail/kinds"
SPIRA_ID_PREFIX="sp"
SPIRA_RUN="$TMP/run"
mkdir -p "$TMP/watchd"
# SPIRA_CONCIERGE_INBOX undeclared resolves to the complete fixture's
# /fixture/userhome/spira/run/watchd/concierge-inbox.log — mail appends every send there, and
# the write fails outright with no such directory (sfail round 3, pattern 7).
# SPIRA_MAIL_MUTE=0: the complete fixture's own declared default is true, which silently
# writes every "rendered block leads the body" message straight to cur/ flagged Seen instead
# of new/ — body_of() only ever looks in new/, so every render assertion read as empty even
# though the block was rendered correctly (verified by hand: the muted file in cur/ carries
# the full "sp-titl01: ... / Status: open / Priority: P3" block).
tl_config SPIRA_MAIL="$SPIRA_MAIL" SPIRA_MAIL_KINDS="$SPIRA_MAIL_KINDS" \
    SPIRA_ID_PREFIX="$SPIRA_ID_PREFIX" SPIRA_RUN="$SPIRA_RUN" \
    SPIRA_MAIL_INDEX="$SPIRA_MAIL/index" SPIRA_MAIL_MUTE=0 \
    SPIRA_CONCIERGE_INBOX="$TMP/watchd/concierge-inbox.log"
export SPIRA_CONF=""
export SPIRA_MAIL_REPEAT_CONSIDERED="test-suite" SPIRA_MAIL_OPERATOR_CONSIDERED="test-suite"
# SPIRA_HOME IS THE HOME now (locate_home no longer searches): every binary reads
# <home>/conf.d (sfail round 2, pattern 1); $HERE already carries the real one.
export SPIRA_HOME="$HERE"

run() { mail "$@"; }

body_of() {
    local mailbox="$1" f
    f="$(ls -t "$SPIRA_MAIL/$mailbox/new"/* 2>/dev/null | head -1)"
    [ -n "$f" ] && sed '1,/^$/d' "$f"
}

KNOWN_ID="sp-titl01"
KNOWN_TITLE="a well-described feature bead with a clear title"
printf '{"id":"%s","title":"%s","status":"open","issue_type":"task","priority":3,"labels":["spira","repo:fixture-repo"],"updated_at":"2026-01-01T00:00:00Z"}\n' \
    "$KNOWN_ID" "$KNOWN_TITLE" | testdb_seed || { echo "test-mail-bead-render: seed failed"; exit 1; }

UNKNOWN_ID="sp-zzz99"

# ==========================================================================
# 1. --bead <id>: rendered block leads the body with title, status, priority.
# ==========================================================================
echo
echo "--bead flag: rendered block leads the body"

out="$(printf 'Some prose about the work, unrelated to the id.\n' \
    | run send operator --from "Builder <builder@spira>" --subject "Status update" \
        --bead "$KNOWN_ID" 2>&1)"; rc=$?
wantrc "--bead: send succeeds" 0 "$rc"
body="$(body_of operator)"
want "block leads with id and title" "$KNOWN_ID: $KNOWN_TITLE" "$body"
want "block carries status"          "Status: open"            "$body"
want "block carries priority"        "Priority: P3"             "$body"
first_line="$(printf '%s\n' "$body" | head -1)"
want "the FIRST line of the body is the rendered block, not the producer's prose" \
    "$KNOWN_ID:" "$first_line"

# ==========================================================================
# 2. unresolvable bead id — renders "unresolved: <id>" and still sends.
#    Paired with case 1: only a REAL lookup can pass both (law-absence-needs-a-
#    positive-control) — a renderer that always emits "unresolved" fails case 1;
#    one that always emits real-looking values fails this case.
# ==========================================================================
echo
echo "unresolvable bead id: renders 'unresolved: <id>', send still succeeds"

out="$(printf 'Blocked on %s until infra is fixed.\n' "$UNKNOWN_ID" \
    | run send operator --from "Builder <builder@spira>" --subject "Infra blocker" \
        --bead "$UNKNOWN_ID" 2>&1)"; rc=$?
wantrc "unresolvable id: send still succeeds (exit 0)" 0 "$rc"
body2="$(body_of operator)"
want "body renders unresolved marker for the unknown id" "unresolved: $UNKNOWN_ID" "$body2"
nowant "unresolved case carries no title (nothing to render)" "$KNOWN_TITLE" "$body2"

# ==========================================================================
# 3. bead id anywhere in body (no --bead) — nothing is rendered.
# ==========================================================================
echo
echo "bead id named only in the body (no --bead): not rendered"

out="$(printf 'The work in %s is blocked on infra.\n' "$KNOWN_ID" \
    | run send operator --from "Builder <builder@spira>" --subject "Infra blocker 2" 2>&1)"; rc=$?
wantrc "body-only id: send succeeds" 0 "$rc"
body3="$(body_of operator)"
nowant "body-only id: no block rendered" "$KNOWN_TITLE" "$body3"

# ==========================================================================
# 4. Regression: the gh-closeout subject shape (lib.sh) — names a bead and nothing
#    else about it. SEEN RED against the pre-change mail (no render step at all,
#    so this assertion fails); SEEN GREEN here.
# ==========================================================================
echo
echo "regression: a gh-closeout-shaped body ('Close GitHub issue ... for bead <id>') renders the bead's title"

REG_ID="sp-ghclose1"
REG_TITLE="artifact deploys cannot reach the forge: every gh call runs from a directory with no .git"
printf '{"id":"%s","title":"%s","status":"closed","issue_type":"bug","priority":1,"labels":["spira"],"updated_at":"2026-01-01T00:00:00Z","closed_at":"2026-01-01T00:00:00Z"}\n' \
    "$REG_ID" "$REG_TITLE" \
    | testdb_seed || { echo "test-mail-bead-render: regression seed failed"; exit 1; }

_subj="Close GitHub issue github:example/repo#1 for bead $REG_ID"
_dflt="post a comment explaining the resolution and close the issue"
out="$(printf '## Question\n%s\n\n## Default\n%s\n\n## Class basis\nneeds a policy ruling\n' "$_subj" "$_dflt" \
    | run send operator --from "Landing gate <gate@spira>" --subject "$_subj" \
        --kind question --class policy --default "$_dflt" --bead "$REG_ID" 2>&1)"; rc=$?
wantrc "regression: send succeeds" 0 "$rc"
body4="$(body_of operator)"
want "regression: rendered block carries the bead's real title" "$REG_TITLE"    "$body4"
want "regression: rendered block carries its status"            "Status: closed" "$body4"

# ==========================================================================
# 5. A prepended title that names a second, closed bead must not borrow that
#    bead's block: --bead names the real subject.
# ==========================================================================
echo
echo "subject whose title names another bead: the --bead block renders, not the stranger's"

OTHER_ID="sp-strng01"; OTHER_TITLE="a closed stranger bead named only inside another title"
printf '{"id":"%s","title":"%s","status":"closed","issue_type":"task","priority":1,"labels":["spira"],"updated_at":"2026-01-01T00:00:00Z","closed_at":"2026-01-01T00:00:00Z"}\n' \
    "$OTHER_ID" "$OTHER_TITLE" \
    | testdb_seed || { echo "test-mail-bead-render: stranger seed failed"; exit 1; }
_subj="Fix found on $OTHER_ID: $KNOWN_ID rebase loop x3"
out="$(printf '## Question\n%s\n\n## Default\nsplit it\n\n## Class basis\nneeds a policy ruling\n' "$_subj" \
    | run send operator --from "Landing gate <gate@spira>" --subject "$_subj" \
        --kind question --class policy --default "split it" --bead "$KNOWN_ID" 2>&1)"; rc=$?
wantrc "stranger-title: send succeeds" 0 "$rc"
body5="$(body_of operator)"
want   "stranger-title: subject bead's block renders" "$KNOWN_ID: $KNOWN_TITLE" "$body5"
nowant "stranger-title: stranger's title never renders" "$OTHER_TITLE" "$body5"

tl_summary
