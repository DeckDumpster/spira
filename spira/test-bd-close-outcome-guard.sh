#!/usr/bin/env bash
#
# test-bd-close-outcome-guard.sh — bd-close-outcome-guard.sh refuses bd close
#   when the close reason does not declare a valid terminal outcome, and validates
#   outcome-specific evidence for escalated and blocked.
#
# POSITIVE CONTROL FIRST (law-absence-needs-a-positive-control): prove the guard
# fires on a plain bd close (no OUTCOME: line) before any negative controls run.
#
# WHAT IS VERIFIED
#  1. POSITIVE CONTROL: no OUTCOME: line → guard blocks (exits 2)
#  2. Invalid outcome type → guard blocks
#  3. submitted with content → allows
#  4. delivered with content → allows
#  5. abandoned with content → allows
#  6. parked with content → allows
#  7. landed → allows
#  8. escalated, no bead ID → blocks
#  9. blocked, no bead ID → blocks
# 10. Non-aeon session → allows
# 11. Non-Bash tool → allows
# 12. Non-close command → allows
# 13. Override env var → allows
# 14. Override inline in command → allows
# 15. No legible reason (no --reason, no heredoc) → blocks
# 16. escalated with ask bead that has this bead as dep → allows  [needs DB]
# 17. escalated with ask bead that has NO dep → blocks             [needs DB]
# 18. blocked with bead ID present → allows                        [needs DB]
# 19. Guard's own filename in a git rm argument list → allows (sp-fjsxb regression)
# 20. Heredoc prose mentioning bd close → allows (sp-fjsxb regression)
#
# covers: spira/bd-close-outcome-guard.sh spira/aeon.sh spira/chamber/builder.md
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"

pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want()   { case "$3" in *"$2"*) ok "$1" ;; *) bad "$1" "wanted [$2] in [$3]"; esac; }
nowant() { case "$3" in *"$2"*) bad "$1" "did not want [$2] in [$3]" ;; *) ok "$1"; esac; }
wantrc() { if [ "$3" -eq "$2" ]; then ok "$1"; else bad "$1" "wanted rc=$2, got rc=$3"; fi; }

echo "test-bd-close-outcome-guard.sh"

GUARD="$HERE/bd-close-outcome-guard.sh"
[ -f "$GUARD" ] || { printf 'SKIP bd-close-outcome-guard.sh not found\n'; exit 77; }

# make_payload <command> — build the PreToolUse JSON for a Bash call.
make_payload() {
    python3 -c 'import json,sys; print(json.dumps({"tool_name":"Bash","tool_input":{"command":sys.argv[1]}})
)' "$1"
}

# run_guard [KEY=VAL ...] — run the guard with standard aeon env, no DB (parsing-only tests).
# Reads the command from $CMD.
run_guard() {
    local rc out
    out=$(make_payload "$CMD" | \
      env -i PATH="$PATH" HOME="$HOME" \
          SPIRA_AEON=1 \
          BEAD_ID="sp-testbead" \
          "$@" \
          bash "$GUARD" 2>&1); rc=$?
    printf '%s' "$out"
    return "$rc"
}

# ============================================================================
echo
echo "POSITIVE CONTROL — no OUTCOME: line → guard blocks"
# ============================================================================
CMD="bd -C /tmp/db close sp-testbead --reason-file - <<'REASON'
Did the work and committed it.
REASON"

rc=0; out="$(run_guard 2>&1 || true)"; run_guard >/dev/null 2>&1 || rc=$?
want   "guard blocks close with no outcome"         "BLOCKED by bd-close-outcome-guard" "$out"
want   "refusal names the valid outcome types"      "submitted"                         "$out"
want   "refusal names the override"                 "BEAD_OUTCOME_CONSIDERED"           "$out"
wantrc "guard exits 2 to block the tool call"       2                                   "$rc"

# ============================================================================
echo
echo "Invalid outcome type → guard blocks"
# ============================================================================
CMD="bd -C /tmp/db close sp-testbead --reason-file - <<'REASON'
OUTCOME: completed
Did the work.
REASON"

rc=0; out="$(run_guard 2>&1 || true)"; run_guard >/dev/null 2>&1 || rc=$?
want   "unknown outcome type is blocked"            "BLOCKED by bd-close-outcome-guard" "$out"
want   "refusal names the bad type"                 "completed"                         "$out"
wantrc "unknown outcome exits 2"                    2                                   "$rc"

# ============================================================================
echo
echo "submitted with content → allows"
# ============================================================================
CMD="bd -C /tmp/db close sp-testbead --reason-file - <<'REASON'
OUTCOME: submitted
Implemented the fix; test-foo.sh green.
REASON"

rc=0; run_guard >/dev/null 2>&1 || rc=$?
wantrc "submitted with content exits 0"             0                                   "$rc"

# ============================================================================
echo
echo "delivered with content → allows"
# ============================================================================
CMD="bd -C /tmp/db close sp-testbead --reason-file - <<'REASON'
OUTCOME: delivered
Filed sp-abc and sp-def for the follow-up work.
REASON"

rc=0; run_guard >/dev/null 2>&1 || rc=$?
wantrc "delivered exits 0"                          0                                   "$rc"

# ============================================================================
echo
echo "abandoned with content → allows"
# ============================================================================
CMD="bd -C /tmp/db close sp-testbead --reason-file - <<'REASON'
OUTCOME: abandoned
The premise was wrong; the issue does not exist.
REASON"

rc=0; run_guard >/dev/null 2>&1 || rc=$?
wantrc "abandoned exits 0"                          0                                   "$rc"

# ============================================================================
echo
echo "parked with content → allows"
# ============================================================================
CMD="bd -C /tmp/db close sp-testbead --reason-file - <<'REASON'
OUTCOME: parked
Ran out of time; still need to update the schema migration.
REASON"

rc=0; run_guard >/dev/null 2>&1 || rc=$?
wantrc "parked exits 0"                             0                                   "$rc"

# ============================================================================
echo
echo "landed → allows"
# ============================================================================
CMD="bd -C /tmp/db close sp-testbead --reason-file - <<'REASON'
OUTCOME: landed
Work confirmed on origin/main via git log.
REASON"

rc=0; run_guard >/dev/null 2>&1 || rc=$?
wantrc "landed exits 0"                             0                                   "$rc"

# ============================================================================
echo
echo "escalated with no bead ID → blocks"
# ============================================================================
CMD="bd -C /tmp/db close sp-testbead --reason-file - <<'REASON'
OUTCOME: escalated
Need operator decision about the database migration approach.
REASON"

rc=0; out="$(run_guard 2>&1 || true)"; run_guard >/dev/null 2>&1 || rc=$?
want   "escalated without bead ID blocked"          "BLOCKED by bd-close-outcome-guard" "$out"
want   "refusal says to name the ask bead"          "ask bead"                          "$out"
wantrc "escalated no-bid exits 2"                   2                                   "$rc"

# ============================================================================
echo
echo "blocked with no bead ID → blocks"
# ============================================================================
CMD="bd -C /tmp/db close sp-testbead --reason-file - <<'REASON'
OUTCOME: blocked
Waiting on the auth refactor to complete before we can continue.
REASON"

rc=0; out="$(run_guard 2>&1 || true)"; run_guard >/dev/null 2>&1 || rc=$?
want   "blocked without bead ID is blocked"         "BLOCKED by bd-close-outcome-guard" "$out"
want   "refusal says to name the blocking bead"     "blocking bead"                     "$out"
wantrc "blocked no-bid exits 2"                     2                                   "$rc"

# ============================================================================
echo
echo "Non-aeon session → allows"
# ============================================================================
CMD="bd -C /tmp/db close sp-testbead --reason done"

rc=0
make_payload "$CMD" | \
  env -i PATH="$PATH" HOME="$HOME" \
      BEAD_ID="sp-testbead" \
      bash "$GUARD" >/dev/null 2>&1 || rc=$?
wantrc "no SPIRA_AEON exits 0"                      0                                   "$rc"

# ============================================================================
echo
echo "Non-Bash tool → allows"
# ============================================================================
rc=0
printf '{"tool_name":"Read","tool_input":{"file_path":"/tmp/x"}}' | \
  env -i PATH="$PATH" HOME="$HOME" SPIRA_AEON=1 BEAD_ID="sp-testbead" \
      bash "$GUARD" >/dev/null 2>&1 || rc=$?
wantrc "non-Bash tool exits 0"                      0                                   "$rc"

# ============================================================================
echo
echo "Non-close Bash command → allows"
# ============================================================================
CMD="bd -C /tmp/db update sp-testbead --status in_progress"

rc=0; run_guard >/dev/null 2>&1 || rc=$?
wantrc "non-close command exits 0"                  0                                   "$rc"

# ============================================================================
echo
echo "Override via environment → allows"
# ============================================================================
CMD="bd -C /tmp/db close sp-testbead --reason done"

rc=0
make_payload "$CMD" | \
  env -i PATH="$PATH" HOME="$HOME" SPIRA_AEON=1 BEAD_ID="sp-testbead" \
      BEAD_OUTCOME_CONSIDERED="transitional close during migration" \
      bash "$GUARD" >/dev/null 2>&1 || rc=$?
wantrc "env override exits 0"                       0                                   "$rc"

# ============================================================================
echo
echo "Override inline in command → allows"
# ============================================================================
CMD="BEAD_OUTCOME_CONSIDERED=migration bd -C /tmp/db close sp-testbead --reason done"

rc=0; run_guard >/dev/null 2>&1 || rc=$?
wantrc "inline override exits 0"                    0                                   "$rc"

# ============================================================================
echo
echo "No legible reason → blocks"
# ============================================================================
# bd close with --reason-file pointing to a non-stdin path has no parseable heredoc body.
CMD="bd -C /tmp/db close sp-testbead --reason-file /tmp/reason.txt"

rc=0; out="$(run_guard 2>&1 || true)"; run_guard >/dev/null 2>&1 || rc=$?
want   "no-reason form is blocked"                  "BLOCKED by bd-close-outcome-guard" "$out"
wantrc "no-reason exits 2"                          2                                   "$rc"

# ============================================================================
echo
echo "Single-quoted prose is not blocked (mention in echo)"
# ============================================================================
CMD="echo 'bd close sp-testbead --reason done'"

rc=0; run_guard >/dev/null 2>&1 || rc=$?
wantrc "single-quoted prose not blocked"            0                                   "$rc"

# ============================================================================
echo
echo "Guard's own filename in a git rm argument list is not blocked"
# ============================================================================
# Regression for sp-fjsxb: "bd-close-outcome-guard.sh" contains "bd" and "close" as
# hyphen-separated substrings a character-level scan cannot tell from a real invocation.
CMD="git rm spira/bd-close-outcome-guard.sh spira/test-bd-close-outcome-guard.sh"

rc=0; run_guard >/dev/null 2>&1 || rc=$?
wantrc "own filename in git rm not blocked"         0                                   "$rc"

# ============================================================================
echo
echo "Heredoc prose mentioning bd close is not blocked"
# ============================================================================
# Regression for sp-fjsxb: a commit message heredoc describing this guard in prose
# ("...in bd close reasons...") is data for git commit, not a bd invocation.
CMD='git commit -m "$(cat <<'"'"'EOF'"'"'
The close-outcome guard enforced an "OUTCOME: <type>" line in bd close reasons.
EOF
)"'

rc=0; run_guard >/dev/null 2>&1 || rc=$?
wantrc "heredoc prose not blocked"                  0                                   "$rc"

# ============================================================================
# DB-dependent tests: escalated/blocked evidence checks
# ============================================================================
. "$HERE/testdb.sh"
if ! testdb_require outcome-guard 2>/dev/null; then
    printf '\nSKIP DB-dependent outcome evidence tests (no fixture available)\n'
else
    TMP="$(mktemp -d)"
    trap 'testdb_drop 2>/dev/null; rm -rf "$TMP"' EXIT INT TERM
    testdb_up outcome-guard || { printf 'SKIP testdb_up failed\n'; }

    if [ -n "${SPIRA_DB:-}" ]; then
        BD="${SPIRA_BD:-bd}"
        ACTOR="aeon-test-outcome"

        # Create the escalating bead (this is BEAD_ID).
        WORK_BID="$("$BD" -C "$SPIRA_DB" create --title "work bead" -l spira --type task \
                    2>/dev/null | grep -oE 'sp-[a-z0-9]+' | head -1)"
        # Create the ask bead (to be named in the escalated reason).
        ASK_BID="$("$BD" -C "$SPIRA_DB" create --title "ask bead" -l spira --type task \
                   2>/dev/null | grep -oE 'sp-[a-z0-9]+' | head -1)"

        if [ -n "$WORK_BID" ] && [ -n "$ASK_BID" ]; then
            BEADS_ACTOR="$ACTOR" "$BD" -C "$SPIRA_DB" update "$WORK_BID" --claim 2>/dev/null

            # ================================================================
            echo
            echo "escalated: ask bead has this bead as dep → allows  [DB]"
            # ================================================================
            # Link: work_bead depends on ask_bead (ask is upstream of work).
            # dep add <A> <B> means A depends on B.
            "$BD" -C "$SPIRA_DB" dep add "$WORK_BID" "$ASK_BID" 2>/dev/null || true

            CMD="bd -C $SPIRA_DB close $WORK_BID --reason-file - <<'REASON'
OUTCOME: escalated
Need operator decision. Filed ask bead $ASK_BID for the question.
REASON"
            rc=0
            make_payload "$CMD" | \
              env -i PATH="$PATH" HOME="$HOME" \
                  SPIRA_AEON=1 BEAD_ID="$WORK_BID" SPIRA_DB="$SPIRA_DB" SPIRA_BD="$BD" \
                  bash "$GUARD" >/dev/null 2>&1 || rc=$?
            wantrc "escalated with dep exits 0"                 0                       "$rc"

            # ================================================================
            echo
            echo "escalated: ask bead has NO dep linking back → blocks  [DB]"
            # ================================================================
            # Create a fresh ask bead with no deps.
            ASK2="$("$BD" -C "$SPIRA_DB" create --title "ask2 no dep" -l spira --type task \
                     2>/dev/null | grep -oE 'sp-[a-z0-9]+' | head -1)"
            [ -n "$ASK2" ] || ASK2="sp-nodep999"

            CMD2="bd -C $SPIRA_DB close $WORK_BID --reason-file - <<'REASON'
OUTCOME: escalated
Need operator decision. Ask bead is $ASK2.
REASON"
            rc=0; out2=""
            out2=$(make_payload "$CMD2" | \
                   env -i PATH="$PATH" HOME="$HOME" \
                       SPIRA_AEON=1 BEAD_ID="$WORK_BID" SPIRA_DB="$SPIRA_DB" SPIRA_BD="$BD" \
                       bash "$GUARD" 2>&1) || rc=$?
            want   "escalated no-dep blocked"        "BLOCKED by bd-close-outcome-guard" "$out2"
            want   "refusal names the ask bead"      "$ASK2"                             "$out2"
            wantrc "escalated no-dep exits 2"        2                                   "$rc"

            # ================================================================
            echo
            echo "blocked: bead ID present in reason → allows  [DB]"
            # ================================================================
            BLOCKER="$("$BD" -C "$SPIRA_DB" create --title "blocker bead" -l spira --type task \
                       2>/dev/null | grep -oE 'sp-[a-z0-9]+' | head -1)"
            [ -n "$BLOCKER" ] || BLOCKER="$ASK_BID"

            CMD3="bd -C $SPIRA_DB close $WORK_BID --reason-file - <<'REASON'
OUTCOME: blocked
Waiting for $BLOCKER (auth refactor) to land before this can proceed.
REASON"
            rc=0
            make_payload "$CMD3" | \
              env -i PATH="$PATH" HOME="$HOME" \
                  SPIRA_AEON=1 BEAD_ID="$WORK_BID" SPIRA_DB="$SPIRA_DB" SPIRA_BD="$BD" \
                  bash "$GUARD" >/dev/null 2>&1 || rc=$?
            wantrc "blocked with bead ID exits 0"   0                                    "$rc"

            # ================================================================
            echo
            echo "submitted on a work-type bead: converted to the label, close refused  [DB]"
            # ================================================================
            # WORK_BID was created --type task, which is in SPIRA_WORK_CLOSE_TYPES by default.
            CMD4="bd -C $SPIRA_DB close $WORK_BID --reason-file - <<'REASON'
OUTCOME: submitted
Implemented the fix; test-foo.sh green.
REASON"
            rc=0; out4=""
            out4=$(make_payload "$CMD4" | \
                   env -i PATH="$PATH" HOME="$HOME" \
                       SPIRA_AEON=1 BEAD_ID="$WORK_BID" SPIRA_DB="$SPIRA_DB" SPIRA_BD="$BD" \
                       bash "$GUARD" 2>&1) || rc=$?
            want   "submitted on a work type is refused"      "BLOCKED by bd-close-outcome-guard" "$out4"
            want   "refusal says the landing pass closes it"  "landing pass"                       "$out4"
            wantrc "submitted on a work type exits 2"         2                                    "$rc"
            lbls4="$("$BD" -C "$SPIRA_DB" show "$WORK_BID" --json 2>/dev/null | python3 -c '
import json, sys
d = json.load(sys.stdin); d = d if isinstance(d, list) else [d]
print(" ".join(d[0].get("labels") or []))' 2>/dev/null)"
            want   "the bead is labelled spira-submitted"     "spira-submitted"                    "$lbls4"
            is_still_open="$("$BD" -C "$SPIRA_DB" show "$WORK_BID" --json 2>/dev/null | python3 -c '
import json, sys
d = json.load(sys.stdin); d = d if isinstance(d, list) else [d]
print(d[0].get("status") or "")' 2>/dev/null)"
            nowant "and the guard did not close it itself"    "closed"                             "$is_still_open"

            # ================================================================
            echo
            echo "submitted on a non-code type bead: closes as before  [DB]"
            # ================================================================
            EVENT_BID="$("$BD" -C "$SPIRA_DB" create --title "an event bead" -l spira --type event \
                         2>/dev/null | grep -oE 'sp-[a-z0-9]+' | head -1)"
            if [ -n "$EVENT_BID" ]; then
                BEADS_ACTOR="$ACTOR" "$BD" -C "$SPIRA_DB" update "$EVENT_BID" --claim 2>/dev/null
                CMD5="bd -C $SPIRA_DB close $EVENT_BID --reason-file - <<'REASON'
OUTCOME: submitted
The event happened; nothing to land.
REASON"
                rc=0
                make_payload "$CMD5" | \
                  env -i PATH="$PATH" HOME="$HOME" \
                      SPIRA_AEON=1 BEAD_ID="$EVENT_BID" SPIRA_DB="$SPIRA_DB" SPIRA_BD="$BD" \
                      bash "$GUARD" >/dev/null 2>&1 || rc=$?
                wantrc "submitted on a non-code type exits 0"     0                                "$rc"
            else
                printf 'SKIP could not create event bead for non-code-type DB test\n'
            fi
        else
            printf 'SKIP could not create test beads for DB tests\n'
        fi
    fi
fi

echo
printf '  %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
