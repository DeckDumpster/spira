#!/usr/bin/env bash
#
# test-bd-close-unacked-guard.sh — bd-close-unacked-guard.sh refuses bd close when
#   a bead has post-claim comments the aeon has not acknowledged, and
#   bd-unacked-comment-deliver.sh delivers those comments as additionalContext.
#
# POSITIVE CONTROL FIRST (law-absence-needs-a-positive-control): prove the guard
# fires on the sp-mh5rm shape before any negative controls run. Shape: bead is
# in_progress, an operator adds a comment after the claim, and the aeon closes
# without an ACK note. That shape currently passes (fails open); the guard closes it.
#
# WHAT IS VERIFIED
# 1. POSITIVE CONTROL: post-claim comment + no ACK → guard blocks (exits 2)
# 2. Failing-open baseline: same scenario, bd close without guard → exits 0
# 3. ACK note present → guard allows (exits 0)
# 4. SPIRA_CLOSE_UNACKED_CONSIDERED override → guard allows
# 5. No post-claim comments → guard allows
# 6. Pre-claim comments only → guard allows
# 7. Non-aeon session (no SPIRA_AEON) → guard allows
# 8. Non-Bash tool call → guard allows
# 9. Non-close Bash command → guard allows
# 10. Comment from aeon itself (BEADS_ACTOR) → no ACK needed
# 11. Deliver hook: new post-claim comment → delivers as additionalContext
# 12. Deliver hook: already-delivered comment → no output (idempotent)
# 13. Deliver hook: pre-claim comment → no output
# 14. Deliver hook: comment from aeon itself → no output
#
# covers: spira/bd-close-unacked-guard.sh spira/bd-unacked-comment-deliver.sh
# covers: spira/aeon.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"

pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want()   { case "$3" in *"$2"*) ok "$1" ;; *) bad "$1" "wanted [$2] in [$3]"; esac; }
nowant() { case "$3" in *"$2"*) bad "$1" "did not want [$2] in [$3]" ;; *) ok "$1"; esac; }
wantrc() { if [ "$3" -eq "$2" ]; then ok "$1"; else bad "$1" "wanted rc=$2, got rc=$3"; fi; }

echo "test-bd-close-unacked-guard.sh"

GUARD="$HERE/bd-close-unacked-guard.sh"
DELIVER="$HERE/bd-unacked-comment-deliver.sh"
[ -f "$GUARD" ]   || { printf 'SKIP bd-close-unacked-guard.sh not found\n'; exit 77; }
[ -f "$DELIVER" ] || { printf 'SKIP bd-unacked-comment-deliver.sh not found\n'; exit 77; }

. "$HERE/testdb.sh"
testdb_require guard-unacked || exit 77
TMP="$(mktemp -d)"
trap 'testdb_drop 2>/dev/null; rm -rf "$TMP"' EXIT INT TERM
testdb_up guard-unacked || exit 1
BD="${SPIRA_BD:-bd}"

# Create a test bead and claim it (set to in_progress which sets started_at).
BID="$("$BD" -C "$SPIRA_DB" create --title "test-close-unacked" -l spira --type task \
        2>/dev/null | grep -oE 'sp-[a-z0-9]+' | head -1)"
[ -n "$BID" ] || { printf 'SKIP could not create test bead\n'; exit 77; }
ACTOR="aeon-test-guard"
BEADS_ACTOR="$ACTOR" "$BD" -C "$SPIRA_DB" update "$BID" --claim 2>/dev/null

# Add a comment AFTER the claim (post-claim comment from an operator).
sleep 2
COMMENT_ID="$("$BD" -C "$SPIRA_DB" comment "$BID" "Amendment: use approach B, not A" \
               2>/dev/null | grep -oE '[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}' | head -1)"
# If bd comment does not print the uuid, fetch it from the list.
if [ -z "$COMMENT_ID" ]; then
    COMMENT_ID="$("$BD" -C "$SPIRA_DB" comments "$BID" --json 2>/dev/null \
                  | python3 -c 'import json,sys; d=json.load(sys.stdin); print(d[0]["id"] if d else "")' 2>/dev/null)"
fi
[ -n "$COMMENT_ID" ] || { printf 'SKIP could not retrieve post-claim comment id\n'; exit 77; }

CLOSE_CMD="bd -C $SPIRA_DB close $BID --reason-file - <<'REASON'
done
REASON"

# make_payload <command> — produce the PreToolUse JSON for a Bash tool call.
make_payload() {
    python3 -c 'import json,sys; print(json.dumps({"tool_name":"Bash","tool_input":{"command":sys.argv[1]}})
)' "$1"
}

# run_guard [KEY=VAL ...] — run the guard with the standard aeon env.
run_guard() {
    local rc out
    out=$(make_payload "$CLOSE_CMD" | \
      env -i PATH="$PATH" HOME="$HOME" \
          SPIRA_AEON=1 \
          BEAD_ID="$BID" \
          BEADS_ACTOR="$ACTOR" \
          SPIRA_DB="$SPIRA_DB" \
          SPIRA_BD="$BD" \
          "$@" \
          bash "$GUARD" 2>&1); rc=$?
    printf '%s' "$out"
    return "$rc"
}

# ============================================================================
echo
echo "POSITIVE CONTROL — sp-mh5rm shape: post-claim comment, no ACK → guard blocks"
# ============================================================================
# This is the shape that produced sp-8ia4q: an operator amendment posted after
# the claim went unacknowledged. The guard MUST fire here.

rc=0; out="$(run_guard 2>&1 || true)"; run_guard >/dev/null 2>&1 || rc=$?
want   "guard blocks close with unacknowledged comment"       "BLOCKED by bd-close-unacked-guard" "$out"
want   "refusal names the unacknowledged comment id"          "$COMMENT_ID"                       "$out"
want   "refusal names the override"                           "SPIRA_CLOSE_UNACKED_CONSIDERED"    "$out"
wantrc "guard exits 2 to block the tool call"                 2                                   "$rc"

# ============================================================================
echo
echo "Failing-open baseline — same scenario, bd close without guard"
# ============================================================================
# The close succeeds when the guard is not in the path. This is the current
# (pre-guard) behaviour that sp-8ia4q exists to close.

rc=0; BEADS_ACTOR="$ACTOR" "$BD" -C "$SPIRA_DB" close "$BID" --reason "done" >/dev/null 2>&1 || rc=$?
wantrc "bd close without guard succeeds (fails open)"         0                                   "$rc"

# Reopen so subsequent tests can close the same bead.
"$BD" -C "$SPIRA_DB" update "$BID" --status in_progress 2>/dev/null

# ============================================================================
echo
echo "ACK note present → guard allows"
# ============================================================================
# Add a ACK note in the bead. The guard reads the notes field for ACK <uuid>: patterns.

"$BD" -C "$SPIRA_DB" note "$BID" "ACK $COMMENT_ID: applied — will use approach B" 2>/dev/null

rc=0; run_guard >/dev/null 2>&1 || rc=$?
wantrc "close with ACK note exits 0"                          0                                   "$rc"
out="$(run_guard 2>&1 || true)"
nowant "close with ACK not blocked"                           "BLOCKED"                           "$out"

# ============================================================================
echo
echo "SPIRA_CLOSE_UNACKED_CONSIDERED env override → guard allows"
# ============================================================================
# Reset: create a new bead with an unacknowledged comment.

BID2="$("$BD" -C "$SPIRA_DB" create --title "test-override" -l spira --type task \
         2>/dev/null | grep -oE 'sp-[a-z0-9]+' | head -1)"
BEADS_ACTOR="$ACTOR" "$BD" -C "$SPIRA_DB" update "$BID2" --claim 2>/dev/null
sleep 2
"$BD" -C "$SPIRA_DB" comment "$BID2" "operator amendment" 2>/dev/null

CLOSE2="bd -C $SPIRA_DB close $BID2 --reason-file - <<'REASON'
done
REASON"

run_guard_bead2() {
    local rc out
    out=$(python3 -c 'import json,sys; print(json.dumps({"tool_name":"Bash","tool_input":{"command":sys.argv[1]}})
)' "$CLOSE2" | \
      env -i PATH="$PATH" HOME="$HOME" \
          SPIRA_AEON=1 \
          BEAD_ID="$BID2" \
          BEADS_ACTOR="$ACTOR" \
          SPIRA_DB="$SPIRA_DB" \
          SPIRA_BD="$BD" \
          "$@" \
          bash "$GUARD" 2>&1); rc=$?
    printf '%s' "$out"
    return "$rc"
}

rc=0; run_guard_bead2 SPIRA_CLOSE_UNACKED_CONSIDERED="set aside for now" >/dev/null 2>&1 || rc=$?
wantrc "env override exits 0"                                 0                                   "$rc"

INLINE_CLOSE2="SPIRA_CLOSE_UNACKED_CONSIDERED=deliberate bd -C $SPIRA_DB close $BID2 --reason done"
rc=0
python3 -c 'import json,sys; print(json.dumps({"tool_name":"Bash","tool_input":{"command":sys.argv[1]}})
)' "$INLINE_CLOSE2" | \
  env -i PATH="$PATH" HOME="$HOME" \
      SPIRA_AEON=1 BEAD_ID="$BID2" BEADS_ACTOR="$ACTOR" SPIRA_DB="$SPIRA_DB" SPIRA_BD="$BD" \
      bash "$GUARD" >/dev/null 2>&1 || rc=$?
wantrc "inline override exits 0"                              0                                   "$rc"

# ============================================================================
echo
echo "No post-claim comments → guard allows"
# ============================================================================

BID3="$("$BD" -C "$SPIRA_DB" create --title "test-no-comments" -l spira --type task \
         2>/dev/null | grep -oE 'sp-[a-z0-9]+' | head -1)"
BEADS_ACTOR="$ACTOR" "$BD" -C "$SPIRA_DB" update "$BID3" --claim 2>/dev/null

CLOSE3="bd -C $SPIRA_DB close $BID3 --reason done"
rc=0
make_payload "$CLOSE3" | \
  env -i PATH="$PATH" HOME="$HOME" \
      SPIRA_AEON=1 BEAD_ID="$BID3" BEADS_ACTOR="$ACTOR" SPIRA_DB="$SPIRA_DB" SPIRA_BD="$BD" \
      bash "$GUARD" >/dev/null 2>&1 || rc=$?
wantrc "no post-claim comments exits 0"                       0                                   "$rc"

# ============================================================================
echo
echo "Pre-claim comments only → guard allows"
# ============================================================================

BID4="$("$BD" -C "$SPIRA_DB" create --title "test-pre-claim" -l spira --type task \
         2>/dev/null | grep -oE 'sp-[a-z0-9]+' | head -1)"
# Add comment BEFORE claim.
"$BD" -C "$SPIRA_DB" comment "$BID4" "pre-claim note" 2>/dev/null
sleep 1
BEADS_ACTOR="$ACTOR" "$BD" -C "$SPIRA_DB" update "$BID4" --claim 2>/dev/null

CLOSE4="bd -C $SPIRA_DB close $BID4 --reason done"
rc=0
make_payload "$CLOSE4" | \
  env -i PATH="$PATH" HOME="$HOME" \
      SPIRA_AEON=1 BEAD_ID="$BID4" BEADS_ACTOR="$ACTOR" SPIRA_DB="$SPIRA_DB" SPIRA_BD="$BD" \
      bash "$GUARD" >/dev/null 2>&1 || rc=$?
wantrc "pre-claim comment only exits 0"                       0                                   "$rc"

# ============================================================================
echo
echo "Non-aeon session → guard allows"
# ============================================================================

rc=0
make_payload "$CLOSE_CMD" | \
  env -i PATH="$PATH" HOME="$HOME" \
      BEAD_ID="$BID" BEADS_ACTOR="$ACTOR" SPIRA_DB="$SPIRA_DB" SPIRA_BD="$BD" \
      bash "$GUARD" >/dev/null 2>&1 || rc=$?
wantrc "no SPIRA_AEON exits 0"                                0                                   "$rc"

# ============================================================================
echo
echo "Non-Bash tool call → guard allows"
# ============================================================================

rc=0
printf '{"tool_name":"Read","tool_input":{"file_path":"/tmp/x"}}' | \
  env -i PATH="$PATH" HOME="$HOME" \
      SPIRA_AEON=1 BEAD_ID="$BID" BEADS_ACTOR="$ACTOR" SPIRA_DB="$SPIRA_DB" SPIRA_BD="$BD" \
      bash "$GUARD" >/dev/null 2>&1 || rc=$?
wantrc "non-Bash tool exits 0"                                0                                   "$rc"

# ============================================================================
echo
echo "Non-close command → guard allows"
# ============================================================================

rc=0
make_payload "bd -C $SPIRA_DB update $BID --status in_progress" | \
  env -i PATH="$PATH" HOME="$HOME" \
      SPIRA_AEON=1 BEAD_ID="$BID" BEADS_ACTOR="$ACTOR" SPIRA_DB="$SPIRA_DB" SPIRA_BD="$BD" \
      bash "$GUARD" >/dev/null 2>&1 || rc=$?
wantrc "bd update not blocked exits 0"                        0                                   "$rc"

# ============================================================================
echo
echo "Comment from aeon itself → no ACK needed"
# ============================================================================

BID5="$("$BD" -C "$SPIRA_DB" create --title "test-self-comment" -l spira --type task \
         2>/dev/null | grep -oE 'sp-[a-z0-9]+' | head -1)"
BEADS_ACTOR="$ACTOR" "$BD" -C "$SPIRA_DB" update "$BID5" --claim 2>/dev/null
sleep 1
# Aeon writes a comment in its own name.
BEADS_ACTOR="$ACTOR" "$BD" -C "$SPIRA_DB" comment "$BID5" "self-written note" 2>/dev/null

CLOSE5="bd -C $SPIRA_DB close $BID5 --reason done"
rc=0
make_payload "$CLOSE5" | \
  env -i PATH="$PATH" HOME="$HOME" \
      SPIRA_AEON=1 BEAD_ID="$BID5" BEADS_ACTOR="$ACTOR" SPIRA_DB="$SPIRA_DB" SPIRA_BD="$BD" \
      bash "$GUARD" >/dev/null 2>&1 || rc=$?
wantrc "aeon's own comment needs no ACK exits 0"              0                                   "$rc"

# ============================================================================
echo
echo "Deliver hook — new post-claim comment is delivered"
# ============================================================================

BID6="$("$BD" -C "$SPIRA_DB" create --title "test-deliver" -l spira --type task \
         2>/dev/null | grep -oE 'sp-[a-z0-9]+' | head -1)"
BEADS_ACTOR="$ACTOR" "$BD" -C "$SPIRA_DB" update "$BID6" --claim 2>/dev/null
sleep 2
C6ID="$("$BD" -C "$SPIRA_DB" comment "$BID6" "please use method X" 2>/dev/null \
        | grep -oE '[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}' | head -1)"
if [ -z "$C6ID" ]; then
    C6ID="$("$BD" -C "$SPIRA_DB" comments "$BID6" --json 2>/dev/null \
            | python3 -c 'import json,sys; d=json.load(sys.stdin); print(d[0]["id"] if d else "")' 2>/dev/null)"
fi
[ -n "$C6ID" ] || { printf 'SKIP could not get deliver test comment id\n'; exit 77; }

STATE6="$TMP/aeon-$BID6-comment-delivered"
out="$(printf '{}' | \
  env -i PATH="$PATH" HOME="$HOME" \
      BEAD_ID="$BID6" BEADS_ACTOR="$ACTOR" \
      SPIRA_DB="$SPIRA_DB" SPIRA_BD="$BD" SPIRA_RUN="$TMP" \
      bash "$DELIVER" 2>/dev/null || true)"
want   "deliver outputs additionalContext"                    "additionalContext"                 "$out"
want   "deliver names the comment id"                        "$C6ID"                             "$out"
want   "deliver names the ACK command"                       "ACK $C6ID"                         "$out"

# ============================================================================
echo
echo "Deliver hook — already-delivered comment is not re-delivered (idempotent)"
# ============================================================================

out2="$(printf '{}' | \
  env -i PATH="$PATH" HOME="$HOME" \
      BEAD_ID="$BID6" BEADS_ACTOR="$ACTOR" \
      SPIRA_DB="$SPIRA_DB" SPIRA_BD="$BD" SPIRA_RUN="$TMP" \
      bash "$DELIVER" 2>/dev/null || true)"
nowant "second deliver is empty (idempotent)"                 "additionalContext"                 "$out2"

# ============================================================================
echo
echo "Deliver hook — pre-claim comment is not delivered"
# ============================================================================

BID7="$("$BD" -C "$SPIRA_DB" create --title "test-deliver-pre" -l spira --type task \
         2>/dev/null | grep -oE 'sp-[a-z0-9]+' | head -1)"
"$BD" -C "$SPIRA_DB" comment "$BID7" "pre-claim comment" 2>/dev/null
sleep 1
BEADS_ACTOR="$ACTOR" "$BD" -C "$SPIRA_DB" update "$BID7" --claim 2>/dev/null

out7="$(printf '{}' | \
  env -i PATH="$PATH" HOME="$HOME" \
      BEAD_ID="$BID7" BEADS_ACTOR="$ACTOR" \
      SPIRA_DB="$SPIRA_DB" SPIRA_BD="$BD" SPIRA_RUN="$TMP" \
      bash "$DELIVER" 2>/dev/null || true)"
nowant "pre-claim comment not delivered"                      "additionalContext"                 "$out7"

# ============================================================================
echo
echo "Deliver hook — comment from aeon itself not delivered"
# ============================================================================

BID8="$("$BD" -C "$SPIRA_DB" create --title "test-deliver-self" -l spira --type task \
         2>/dev/null | grep -oE 'sp-[a-z0-9]+' | head -1)"
BEADS_ACTOR="$ACTOR" "$BD" -C "$SPIRA_DB" update "$BID8" --claim 2>/dev/null
sleep 1
BEADS_ACTOR="$ACTOR" "$BD" -C "$SPIRA_DB" comment "$BID8" "self comment" 2>/dev/null

out8="$(printf '{}' | \
  env -i PATH="$PATH" HOME="$HOME" \
      BEAD_ID="$BID8" BEADS_ACTOR="$ACTOR" \
      SPIRA_DB="$SPIRA_DB" SPIRA_BD="$BD" SPIRA_RUN="$TMP" \
      bash "$DELIVER" 2>/dev/null || true)"
nowant "aeon self-comment not delivered"                      "additionalContext"                 "$out8"

echo
printf '  %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
