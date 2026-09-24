#!/usr/bin/env bash
# test-aeon-decision-blocked.sh — aeon exit with an open decision blocker is released
#   without charging an attempt toward poison.
#
# Acceptance criterion (d) from sp-egge2: blocked-on-decision exits do not poison.
#
# THE DEFECT THIS TESTS. An aeon that mailed a question about a bead and exited non-zero
# was counted as a failed attempt. After three attempts the sentinel poisoned the bead,
# even though the aeon did exactly the right thing: it filed a decision bead as a blocker
# and exited, leaving the work bead not-ready until the operator replied.
#
# WHAT IS TESTED:
#   1. A work bead blocked by an open decision bead (needs-operator label): the aeon that
#      exits without closing it is released, no attempt charged.
#      POSITIVE CONTROL: a work bead with NO decision blocker: the aeon that exits without
#      closing it IS charged an attempt (the normal case).
#
# Driven through the real aeon.sh with a shim standing in for the model.
#
# defect: sp-egge2
# covers: spira/aeon.sh spira/lib.sh spira/mail.sh
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
testdb_require test-aeon-decision-blocked
TMP="$(mktemp -d)"
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up aeon-dec-blocked || { echo "test-aeon-decision-blocked: could not build fixture database"; exit 1; }
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

ORIGIN="$TMP/origin.git"; git init -q --bare -b main "$ORIGIN"
REPO="$TMP/repo"; git clone -q "$ORIGIN" "$REPO" 2>/dev/null
git -C "$REPO" config user.email t@t; git -C "$REPO" config user.name t
printf 'seed\n' > "$REPO/f"
git -C "$REPO" add f; git -C "$REPO" commit -qm seed; git -C "$REPO" push -q origin main 2>/dev/null

export SPIRA_HOME="$TMP/home"; mkdir -p "$SPIRA_HOME/chamber"
export SPIRA_RUN="$TMP/run"; mkdir -p "$SPIRA_RUN"
export SPIRA_REPO_MAP="$TMP/repo-map"
printf 'fixture | %s | push | origin/main | |\n' "$REPO" > "$SPIRA_REPO_MAP"
cat > "$SPIRA_HOME/chamber/builder.fayth" <<FAYTH
FAYTH_NAME=builder
FAYTH_LABELS="\${SPIRA_SCOPE_LABEL:+\${SPIRA_SCOPE_LABEL},}\${SPIRA_PLAN_LABEL}"
FAYTH_EXCLUDE_LABELS="spira-poison,${SPIRA_ASK_LABEL:-needs-operator}"
FAYTH_MAX_CONCURRENT=1
FAYTH_HEARTBEAT_SECONDS=600
FAYTH
printf 'work {{BEAD_ID}} in {{REPO}} on {{BRANCH}}\n{{PARK}}\n' > "$SPIRA_HOME/chamber/builder.md"

BIN="$TMP/bin"; mkdir -p "$BIN"; export SPIRA_AGENT="$BIN/claude" TMP
grep -q 'SPIRA_AGENT' "$HERE/aeon.sh" \
    || { echo "test-aeon-decision-blocked: aeon.sh has no SPIRA_AGENT injection — refusing to run the real model" >&2; exit 1; }

# Shim A (positive control): exits non-zero without creating a decision dep.
# Used for case 1 to confirm a bare exit charges an attempt.
cat > "$BIN/claude-no-dec" <<'SHIM'
#!/usr/bin/env bash
cat /dev/stdin > /dev/null
printf '{"type":"assistant","message":{"id":"m1","content":[{"type":"tool_use","name":"Bash","input":{"command":"true"}}]}}\n'
printf '{"type":"result","subtype":"success","is_error":false,"duration_ms":1000,"num_turns":1,"total_cost_usd":0.001}\n'
exit 1
SHIM
chmod +x "$BIN/claude-no-dec"

# Shim B (decision-blocked case): creates a decision bead blocking the claimed bead,
# then exits non-zero — simulating an aeon that filed a question via mail.sh for a
# decision bead (blocking edge is correct when the cited bead is a decision).
# The dep is added AFTER the bead is claimed (in_progress); bd ready only returns
# unblocked beads, so a pre-existing dep would prevent the claim entirely.
cat > "$BIN/claude-with-dec" <<'SHIM'
#!/usr/bin/env bash
cat /dev/stdin > /dev/null
printf '{"type":"assistant","message":{"id":"m1","content":[{"type":"tool_use","name":"Bash","input":{"command":"true"}}]}}\n'
_bd="${SPIRA_BD:-bd}"
id="$(BD_IGNORE_SCHEMA_SKEW=1 "$_bd" -C "$SPIRA_DB" list --json 2>/dev/null \
    | python3 -c 'import json,sys; r=json.load(sys.stdin); r=r if isinstance(r,list) else [r]; \
      print(next((x["id"] for x in r if x.get("status")=="in_progress"),""))' 2>/dev/null)"
if [ -n "$id" ]; then
    BD_IGNORE_SCHEMA_SKEW=1 "$_bd" -C "$SPIRA_DB" create \
        "Operator question about $id" \
        -l "${SPIRA_ASK_LABEL:-needs-operator},overseer" \
        --type decision \
        --deps "blocks:$id" \
        --silent >/dev/null 2>&1 || true
fi
printf '{"type":"result","subtype":"success","is_error":false,"duration_ms":1000,"num_turns":1,"total_cost_usd":0.001}\n'
exit 1
SHIM
chmod +x "$BIN/claude-with-dec"

# Shim C (relates-to case): creates a decision bead and links it via bd dep relate,
# mirroring what mail.sh does when the cited bead is a non-decision task.  The
# relates-to edge must NOT be treated as a blocker by aeon.sh — the bead must be
# worked (attempt charged), not released.
cat > "$BIN/claude-with-relates" <<'SHIM'
#!/usr/bin/env bash
cat /dev/stdin > /dev/null
printf '{"type":"assistant","message":{"id":"m1","content":[{"type":"tool_use","name":"Bash","input":{"command":"true"}}]}}\n'
_bd="${SPIRA_BD:-bd}"
id="$(BD_IGNORE_SCHEMA_SKEW=1 "$_bd" -C "$SPIRA_DB" list --json 2>/dev/null \
    | python3 -c 'import json,sys; r=json.load(sys.stdin); r=r if isinstance(r,list) else [r]; \
      print(next((x["id"] for x in r if x.get("status")=="in_progress"),""))' 2>/dev/null)"
if [ -n "$id" ]; then
    # bd q outputs the bare id — use it so we can wire the dep in a second call.
    dec_id="$(BD_IGNORE_SCHEMA_SKEW=1 "$_bd" -C "$SPIRA_DB" q \
        "Relates-to question about $id" \
        -l "${SPIRA_ASK_LABEL:-needs-operator},overseer" \
        --type decision 2>/dev/null)" || dec_id=""
    if [ -n "$dec_id" ]; then
        # Mirror mail.sh: dep relate creates the bidirectional relates_to link.
        BD_IGNORE_SCHEMA_SKEW=1 "$_bd" -C "$SPIRA_DB" dep relate "$dec_id" "$id" >/dev/null 2>&1 || true
    fi
fi
printf '{"type":"result","subtype":"success","is_error":false,"duration_ms":1000,"num_turns":1,"total_cost_usd":0.001}\n'
exit 1
SHIM
chmod +x "$BIN/claude-with-relates"

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

echo "test-aeon-decision-blocked.sh"

# ======================================================================================
# POSITIVE CONTROL: without a decision blocker, exit without closing IS charged.
# ======================================================================================
echo
echo "positive control — exit without closing, no decision blocker — attempt IS charged"

ln -sf "$BIN/claude-no-dec" "$BIN/claude"
fresh; seed sp-db-1
rc="$(run_aeon)"
is "SEEN RED: bead is still open (never closed)"  "open" "$(bead_status sp-db-1)"
notes1="$(bead_notes sp-db-1)"
want "SEEN RED: note says Unlanded (attempt charged)" "Unlanded" "$notes1"

# ======================================================================================
# CASE: with an open decision blocker — exit without closing is NOT charged.
# ======================================================================================
echo
echo "blocked by open decision bead — exit without closing, no attempt charged"

ln -sf "$BIN/claude-with-dec" "$BIN/claude"
fresh; seed sp-db-2
# No pre-existing decision dep — the shim creates it during the session after claiming
# the bead. bd ready only returns unblocked beads, so the dep must be added post-claim.
rc="$(run_aeon)"
is "bead is still open (correctly not closed)" "open" "$(bead_status sp-db-2)"
notes2="$(bead_notes sp-db-2)"
want "note says released due to decision blocker" "decision" "$notes2"
want "note says no attempt charged" "No attempt charged" "$notes2"
lacks "note does not say Unlanded" "Unlanded" "$notes2"
want "ledger says decision-blocked" "decision-blocked" \
    "$(grep 'done builder sp-db-2' "$SPIRA_RUN/aeon-ledger.log" 2>/dev/null)"

# ======================================================================================
# CASE: relates-to ask dep — must be WORKED (attempt charged), not released.
# This is the sp-dvsqc defect: aeon.sh ignored dependency_type and treated every
# open ask-labelled dep as a blocker, including relates-to edges wired by mail.sh.
# ======================================================================================
echo
echo "SEEN RED (unfixed): ask-labelled dep via relates-to edge — must charge attempt, not release"

ln -sf "$BIN/claude-with-relates" "$BIN/claude"
fresh; seed sp-db-3
rc="$(run_aeon)"
notes3="$(bead_notes sp-db-3)"
want "SEEN RED: attempt IS charged (Unlanded, not released)" "Unlanded" "$notes3"
lacks "SEEN RED: not released as decision-blocked" "No attempt charged" "$notes3"
lacks "SEEN RED: ledger must not say decision-blocked" "decision-blocked" \
    "$(grep 'done builder sp-db-3' "$SPIRA_RUN/aeon-ledger.log" 2>/dev/null)"

echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
