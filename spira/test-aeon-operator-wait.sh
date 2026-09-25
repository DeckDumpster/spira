#!/usr/bin/env bash
# test-aeon-operator-wait.sh — a session that sends kind-question mail to the operator
#   and exits is not charged an attempt toward poison.
#
# THE DEFECT THIS TESTS. A session that mailed the concierge for a workflow dispatch
# (or sent any kind-question mail) and exited was charged as a failed attempt. Three
# such waits poisoned the bead even though the aeon did the right thing: it filed the
# question and exited to wait for an answer. The bead was not wrong; the operator was
# still deciding.
#
# WHAT IS TESTED:
#   1. POSITIVE CONTROL: a session that exits with the bead open and no kind-question
#      mail is charged an attempt (the normal unlanded case).
#   2. A session that writes the operator-wait marker (as mail.sh does after sending
#      kind-question or kind-decision mail) and exits is released, no attempt charged.
#   3. mail.sh writes the operator-wait marker when kind=question is sent to operator
#      in an aeon context (BEAD_ID and SPIRA_RUN set).
#
# defect: sp-ne93n
# covers: spira/aeon.sh spira/mail.sh spira/lib.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()   { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()  { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()   { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want() { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
lacks(){ [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-aeon-operator-wait
TMP="$(mktemp -d)"
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up aeonopwait || { echo "test-aeon-operator-wait: could not build fixture database"; exit 1; }
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

ORIGIN="$TMP/origin.git"; git init -q --bare -b main "$ORIGIN"
REPO="$TMP/repo"; git clone -q "$ORIGIN" "$REPO" 2>/dev/null
git -C "$REPO" config user.email t@t; git -C "$REPO" config user.name t
printf 'seed\n' > "$REPO/f"
git -C "$REPO" add f; git -C "$REPO" commit -qm seed; git -C "$REPO" push -q origin main 2>/dev/null

export SPIRA_HOME="$TMP/home"; mkdir -p "$SPIRA_HOME/chamber"
export SPIRA_RUN="$TMP/run"; mkdir -p "$SPIRA_RUN"
export SPIRA_MAIL="$TMP/mail"
export SPIRA_MAIL_KINDS="$HERE/mail/kinds"
export SPIRA_REPO_MAP="$TMP/repo-map"
printf 'fixture | %s | push | origin/main | |\n' "$REPO" > "$SPIRA_REPO_MAP"
cat > "$SPIRA_HOME/chamber/builder.fayth" <<FAYTH
FAYTH_NAME=builder
FAYTH_LABELS="${SPIRA_SCOPE_LABEL:+${SPIRA_SCOPE_LABEL},}${SPIRA_PLAN_LABEL:-plan}"
FAYTH_EXCLUDE_LABELS="spira-poison,${SPIRA_ASK_LABEL:-needs-operator}"
FAYTH_MAX_CONCURRENT=1
FAYTH_HEARTBEAT_SECONDS=600
FAYTH
printf 'work {{BEAD_ID}} in {{REPO}} on {{BRANCH}}\n{{PARK}}\n' > "$SPIRA_HOME/chamber/builder.md"

BIN="$TMP/bin"; mkdir -p "$BIN"; export SPIRA_AGENT="$BIN/claude" TMP
grep -q 'SPIRA_AGENT' "$HERE/aeon.sh" \
    || { echo "test-aeon-operator-wait: aeon.sh has no SPIRA_AGENT injection — refusing to run the real model" >&2; exit 1; }

# Shim A (positive control): exits without sending mail — attempt IS charged.
cat > "$BIN/claude-no-mail" <<'SHIM'
#!/usr/bin/env bash
cat /dev/stdin > /dev/null
printf '{"type":"assistant","message":{"id":"m1","content":[{"type":"tool_use","name":"Bash","input":{"command":"true"}}]}}\n'
printf '{"type":"result","subtype":"success","is_error":false,"duration_ms":1000,"num_turns":1,"total_cost_usd":0.001}\n'
exit 0
SHIM
chmod +x "$BIN/claude-no-mail"

# Shim B: writes the operator-wait marker (simulating mail.sh sending kind-question),
# then exits with the bead open — no attempt should be charged.
cat > "$BIN/claude-with-mail" <<'SHIM'
#!/usr/bin/env bash
cat /dev/stdin > /dev/null
printf '{"type":"assistant","message":{"id":"m1","content":[{"type":"tool_use","name":"Bash","input":{"command":"true"}}]}}\n'
if [ -n "${BEAD_ID:-}" ] && [ -n "${SPIRA_RUN:-}" ]; then
    touch "$SPIRA_RUN/$BEAD_ID.operator-wait"
fi
printf '{"type":"result","subtype":"success","is_error":false,"duration_ms":1000,"num_turns":1,"total_cost_usd":0.001}\n'
exit 0
SHIM
chmod +x "$BIN/claude-with-mail"

seed() {
    local _lbl="${SPIRA_SCOPE_LABEL:+\"${SPIRA_SCOPE_LABEL}\",}\"${SPIRA_PLAN_LABEL:-plan}\",\"repo:fixture\""
    printf '{"id":"%s","title":"t","status":"open","issue_type":"task","labels":[%s],"updated_at":"2026-09-04T00:00:00Z"}\n' \
        "$1" "$_lbl" | testdb_seed
}
run_aeon() {
    rm -rf "$SPIRA_RUN/worktree"
    "$HERE/aeon.sh" builder > "$TMP/out" 2>&1
    echo $?
}
bead_status() {
    BD_IGNORE_SCHEMA_SKEW=1 bd -C "$SPIRA_DB" show "$1" --json 2>/dev/null \
        | python3 -c '
import sys, json
d = json.load(sys.stdin)
d = d if isinstance(d, list) else [d]
print(d[0].get("status", ""))' 2>/dev/null
}
bead_notes() {
    BD_IGNORE_SCHEMA_SKEW=1 bd -C "$SPIRA_DB" show "$1" --json 2>/dev/null \
        | python3 -c '
import sys, json
d = json.load(sys.stdin)
d = d if isinstance(d, list) else [d]
print(d[0].get("notes", "") or "")' 2>/dev/null
}
fresh() { testdb_reset; }

echo "test-aeon-operator-wait.sh"

# ======================================================================================
# POSITIVE CONTROL: without sending mail, exit with bead open IS charged.
# ======================================================================================
echo
echo "positive control — exit without operator mail — attempt IS charged"

ln -sf "$BIN/claude-no-mail" "$BIN/claude"
fresh; seed sp-ow-1
run_aeon
is   "SEEN RED: bead is still open"      "open"    "$(bead_status sp-ow-1)"
want "SEEN RED: note says Unlanded"      "Unlanded" "$(bead_notes sp-ow-1)"

# ======================================================================================
# CASE: with operator-wait marker — exit without closing is NOT charged.
# ======================================================================================
echo
echo "operator-wait marker present — exit without closing, no attempt charged"

ln -sf "$BIN/claude-with-mail" "$BIN/claude"
fresh; seed sp-ow-2
run_aeon
is   "bead is still open (correctly not closed)" "open" "$(bead_status sp-ow-2)"
notes2="$(bead_notes sp-ow-2)"
want "note says kind-question mail"      "kind-question mail" "$notes2"
want "note says No attempt charged"      "No attempt charged" "$notes2"
lacks "note does not say Unlanded"       "Unlanded"           "$notes2"
want "ledger says operator-wait" "operator-wait" \
    "$(grep 'done builder sp-ow-2' "$SPIRA_RUN/aeon-ledger.log" 2>/dev/null)"

# ======================================================================================
# Code assertion: mail.sh writes the operator-wait marker for kind=question/decision.
# ======================================================================================
echo
echo "mail.sh code: writes operator-wait marker when kind=question/decision"

is "mail.sh writes operator-wait for kind=question" "1" \
    "$(grep -c 'BEAD_ID.*operator-wait\|operator-wait.*BEAD_ID' "$HERE/mail.sh" 2>/dev/null || true)"

# The decision-blocked path's requeue cause moved into aeon_disposition's own output
# (lib.sh) as part of sp-eq8a4.2.1, so "unjudged-decision-blocked" is no longer a literal
# beside a bump_requeue call in aeon.sh; behaviour is covered by test-aeon-disposition.sh's
# "open decision blocker is free" row instead of a source-order grep here.

echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
