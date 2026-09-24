#!/usr/bin/env bash
#
# test-rebase-escalation.sh — spira_ask_rebase_loop builds an escalation the operator
# can act on: title in subject, status in body, no blank slot in default when others
# is empty, and ask_already_open dedupe unchanged.
#
# ACCEPTANCE CRITERIA (from sp-lyw8z):
#   1. others empty  → default contains no blank slot (no "duplicate of  and close")
#   2. closed bead   → body carries the bead status
#   3. others present → names those beads (positive control for law-absence-needs-a-positive-control)
#   4. open ask exists → suppresses duplicate (dedupe unchanged)
#
# A REAL bd ON A FIXTURE DATABASE. ask_already_open and the bead title/status lookup
# both query the database; a stub would model the surface and drift silently.
#
# covers: spira/lib.sh spira/landing.sh
# hermetic-ok: uses a fixture database, stub mail.sh, no systemd or gh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-rebase-escalation
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up rebase-escalation || { echo "test-rebase-escalation: could not build a fixture database"; exit 1; }

# Stub mail.sh: capture full args and body so assertions read the rendered content.
export MAIL_LOG="$TMP/mail.log"
cat > "$TMP/mail.sh" <<'MAILSH'
#!/usr/bin/env bash
body="$(cat)"
{
    printf '=== ARGS ===\n'
    for a in "$@"; do printf '%s\n' "$a"; done
    printf '=== BODY ===\n'
    printf '%s\n' "$body"
    printf '=== END ===\n'
} >> "$MAIL_LOG"
MAILSH
chmod +x "$TMP/mail.sh"

RUN="$TMP/run"; mkdir -p "$RUN"
export SPIRA_HOME="$TMP"
export SPIRA_RUN="$RUN"

# shellcheck disable=SC1090
. "$HERE/lib.sh"

B() { bd -C "$SPIRA_DB" "$@"; }

# Read captured body from the stub log (content between last === BODY === and === END ===).
last_body() {
    awk '
        /^=== BODY ===$/ { p=1; buf=""; next }
        /^=== END ===$/ { last=buf; p=0; next }
        p { buf = buf $0 "\n" }
        END { printf "%s", last }
    ' "$MAIL_LOG"
}

# Read captured default (the value after --default in args).
last_default() {
    awk '
        /^=== ARGS ===$/ { in_args=1; want_next=0; d=""; next }
        /^=== END ===$/ { last=d; in_args=0; next }
        in_args && /^--default$/ { want_next=1; next }
        in_args && want_next { d=$0; want_next=0; next }
        { next }
        END { printf "%s", last }
    ' "$MAIL_LOG"
}

echo "test-rebase-escalation.sh"

# ======================================================================================
echo
echo "empty others — default has no blank slot:"
# ======================================================================================
# POSITIVE CONTROL: a subject that would have had a blank slot in the old code must
# not have one in the new code (law-absence-needs-a-positive-control proved below).
testdb_reset
printf '{"id":"sp-t001","title":"express lane: exempts critical beads","status":"in_progress","issue_type":"task","labels":["plan"],"updated_at":"2026-09-01T00:00:00Z"}\n' \
    | testdb_seed
: > "$MAIL_LOG"
spira_ask_rebase_loop "sp-t001" "spira/sp-t001" "spira" "3" "foo.sh" ""

body="$(last_body)"
dflt="$(last_default)"
# The old code rendered "of  and close" when others was empty.
nowant "no 'duplicate of' clause" "is a duplicate of" "$body"
nowant "no blank slot in default" "duplicate of" "$dflt"
want   "default says rebase by hand" "by hand and push" "$dflt"

# ======================================================================================
echo
echo "positive control — non-empty others names those beads:"
# ======================================================================================
testdb_reset
printf '{"id":"sp-t002","title":"some open task","status":"in_progress","issue_type":"task","labels":["plan"],"updated_at":"2026-09-01T00:00:00Z"}\n' \
    | testdb_seed
: > "$MAIL_LOG"
spira_ask_rebase_loop "sp-t002" "spira/sp-t002" "spira" "4" "bar.sh" "sp-other1 sp-other2"

body="$(last_body)"
dflt="$(last_default)"
want "names first other bead"  "sp-other1" "$body"
want "names second other bead" "sp-other2" "$body"
want "default includes duplicate clause" "is a duplicate of" "$dflt"

# ======================================================================================
echo
echo "closed bead — status appears in body:"
# ======================================================================================
testdb_reset
printf '{"id":"sp-t003","title":"a closed task","status":"closed","issue_type":"task","labels":["plan"],"updated_at":"2026-09-01T00:00:00Z"}\n' \
    | testdb_seed
: > "$MAIL_LOG"
spira_ask_rebase_loop "sp-t003" "spira/sp-t003" "spira" "5" "baz.sh" ""

body="$(last_body)"
want "closed status in body" "Status: closed" "$body"

# ======================================================================================
echo
echo "open ask suppresses duplicate — dedupe unchanged:"
# ======================================================================================
# POSITIVE CONTROL: without suppression, mail IS sent when no ask is open.
testdb_reset
printf '{"id":"sp-t004","title":"dedupe subject task","status":"in_progress","issue_type":"task","labels":["plan"],"updated_at":"2026-09-01T00:00:00Z"}\n' \
    | testdb_seed
: > "$MAIL_LOG"
spira_ask_rebase_loop "sp-t004" "spira/sp-t004" "spira" "3" "qux.sh" ""
want "no open ask: mail is sent" "=== BODY ===" "$(cat "$MAIL_LOG")"

# Suppression: an open ask for this branch prevents re-asking.
# THE FIXTURE LABEL MUST MATCH SPIRA_ASK_LABEL so ask_already_open finds the bead.
printf '{"id":"sp-ask1","title":"spira/sp-t004 rebase loop x3 in spira","status":"open","issue_type":"decision","labels":["%s"],"updated_at":"2026-09-01T00:00:00Z"}\n' \
    "${SPIRA_ASK_LABEL:-needs-operator}" | testdb_seed
: > "$MAIL_LOG"
spira_ask_rebase_loop "sp-t004" "spira/sp-t004" "spira" "4" "qux.sh" ""
is "open ask suppresses duplicate" "" "$(cat "$MAIL_LOG")"

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
