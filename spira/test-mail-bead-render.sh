#!/usr/bin/env bash
# test-mail-bead-render.sh — mail.sh renders a bead block for every bead it is
# given, so a message that names a bead cannot lack its details
# (law-a-bead-reference-carries-its-details).
#
# Three ways a bead reaches the renderer, and what happens when the store can't
# resolve it:
#   1. --bead <id>            — resolved and rendered, above the producer's prose.
#   2. id anywhere in the body — resolved the same way; no --bead required.
#   3. an id the store cannot resolve — renders "unresolved: <id>"; send still succeeds.
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
# that names a bead and says nothing about it. Against mail.sh before this change, no
# render step exists at all, so this assertion fails; verified by hand against the
# pre-change mail.sh.
#
# covers: spira/mail.sh spira/lib.sh spira/conf.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
pass=0; fail=0
ok()   { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()  { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
isz()  { [ "$2" = 0 ]  && ok "$1" || bad "$1" "wanted exit 0, got $2"; }
want() { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
lacks(){ [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

echo "test-mail-bead-render.sh"

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-mail-bead-render
TMP="$(mktemp -d)"
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up mail-bead-render || { echo "test-mail-bead-render: could not build fixture database"; exit 1; }

export SPIRA_MAIL="$TMP/mail"
export SPIRA_MAIL_KINDS="$HERE/mail/kinds"
export SPIRA_CONF=""
export SPIRA_ID_PREFIX="sp"
export SPIRA_RUN="$TMP/run"
export SPIRA_MAIL_REPEAT_CONSIDERED="test-suite"

run() { bash "$HERE/mail.sh" "$@"; }

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
isz "--bead: send succeeds" "$rc"
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
    | run send operator --from "Builder <builder@spira>" --subject "Infra blocker" 2>&1)"; rc=$?
isz "unresolvable id: send still succeeds (exit 0)" "$rc"
body2="$(body_of operator)"
want "body renders unresolved marker for the unknown id" "unresolved: $UNKNOWN_ID" "$body2"
lacks "unresolved case carries no title (nothing to render)" "$KNOWN_TITLE" "$body2"

# ==========================================================================
# 3. bead id anywhere in body (no --bead) — resolved and rendered the same way.
# ==========================================================================
echo
echo "bead id named only in the body (no --bead): resolved and rendered"

out="$(printf 'The work in %s is blocked on infra.\n' "$KNOWN_ID" \
    | run send operator --from "Builder <builder@spira>" --subject "Infra blocker 2" 2>&1)"; rc=$?
isz "body-only id: send succeeds" "$rc"
body3="$(body_of operator)"
want "body-only id: rendered block present with real title" "$KNOWN_ID: $KNOWN_TITLE" "$body3"

# ==========================================================================
# 4. Regression: the gh-closeout subject shape (lib.sh) — names a bead and nothing
#    else about it. SEEN RED against the pre-change mail.sh (no render step at all,
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
out="$(printf '## Question\n%s\n\n## Default\n%s\n' "$_subj" "$_dflt" \
    | run send operator --from "Landing gate <gate@spira>" --subject "$_subj" \
        --kind question --default "$_dflt" --bead "$REG_ID" 2>&1)"; rc=$?
isz "regression: send succeeds" "$rc"
body4="$(body_of operator)"
want "regression: rendered block carries the bead's real title" "$REG_TITLE"    "$body4"
want "regression: rendered block carries its status"            "Status: closed" "$body4"

echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
