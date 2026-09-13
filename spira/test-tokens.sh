#!/usr/bin/env bash
#
# test-tokens.sh — the token split attributes worktree transcripts to aeons.
#
#   ./test-tokens.sh
#
# A transcript whose project directory encodes a path under $SPIRA_RUN/worktree
# belongs to an aeon, not to the interactive session. This suite verifies that
# attribution, the dedupe across aeon logs and worktree transcripts, and the
# split the pane renders.
#
# No database, no network, under a second.
#
# defect: sp-rcr
# covers: spira/tokens.sh
# scar: worktree transcripts were attributed to the interactive session rather than to the aeons that produced them; the token split on the pane collapsed all aeon work into operator spend.
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()  { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }

echo "test-tokens.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
RUN="$TMP/run"
PROJ="$TMP/projects"
mkdir -p "$RUN" "$PROJ"

# ---------------------------------------------------------------------------
# The Claude client encodes project directories by replacing / and . with -.
# Build the worktree transcript directory from the RUN path so the test works
# wherever $TMP lands.
# ---------------------------------------------------------------------------
WT_DIR_NAME="$(echo "$RUN/worktree/sp-test" | sed 's|[/.]|-|g')"
SESS_DIR_NAME="-home-user-brain"
mkdir -p "$PROJ/$WT_DIR_NAME" "$PROJ/$SESS_DIR_NAME"

# ---------------------------------------------------------------------------
# Fixture: three inputs, four unique message ids
#
#   aeon log:              msg_shared, msg_aeon_only
#   worktree transcript:   msg_shared, msg_wt_only
#   session transcript:    msg_session
#
# msg_shared appears in both the aeon log and the worktree transcript; after
# dedupe it must be counted exactly once, under aeons.
# ---------------------------------------------------------------------------
NOW="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

cat > "$RUN/sp-test.log" <<AEONLOG
{"type":"system","subtype":"init","session_id":"s1"}
{"type":"assistant","message":{"id":"msg_shared","usage":{"input_tokens":100,"cache_creation_input_tokens":200,"cache_read_input_tokens":300,"output_tokens":50}},"timestamp":"$NOW"}
{"type":"assistant","message":{"id":"msg_aeon_only","usage":{"input_tokens":100,"cache_creation_input_tokens":200,"cache_read_input_tokens":300,"output_tokens":50}},"timestamp":"$NOW"}
AEONLOG

cat > "$PROJ/$WT_DIR_NAME/transcript.jsonl" <<WTJSONL
{"type":"assistant","message":{"id":"msg_shared","usage":{"input_tokens":100,"cache_creation_input_tokens":200,"cache_read_input_tokens":300,"output_tokens":50}},"timestamp":"$NOW"}
{"type":"assistant","message":{"id":"msg_wt_only","usage":{"input_tokens":100,"cache_creation_input_tokens":200,"cache_read_input_tokens":300,"output_tokens":50}},"timestamp":"$NOW"}
WTJSONL

cat > "$PROJ/$SESS_DIR_NAME/session.jsonl" <<SESSJSONL
{"type":"assistant","message":{"id":"msg_session","usage":{"input_tokens":100,"cache_creation_input_tokens":200,"cache_read_input_tokens":300,"output_tokens":50}},"timestamp":"$NOW"}
SESSJSONL

# ---------------------------------------------------------------------------
run_tokens() {
    env -i PATH="$PATH" HOME="$TMP" \
        SPIRA_RUN="$RUN" SPIRA_TOKEN_PROJECTS="$PROJ" \
        SPIRA_TOKEN_WINDOW_H=87600 SPIRA_CONF="$TMP/no.conf" \
        bash "$HERE/tokens.sh" "$1" 2>/dev/null
}

ENV_OUT="$(run_tokens env)"
field() { sed -n "s/^$1=//p" <<< "$ENV_OUT"; }

# The shared turn is counted once, under aeons. The worktree-only turn is also
# under aeons. Four unique turns across three inputs: 3 aeon, 1 session.
is "aeon turns (log + worktree, deduped)" "3" "$(field SP_TOK_AEON_TURNS)"
is "session turns (non-worktree only)"    "1" "$(field SP_TOK_SESS_TURNS)"

# No turn in both: aeon + session equals the four unique messages.
TOTAL=$(( $(field SP_TOK_AEON_TURNS) + $(field SP_TOK_SESS_TURNS) ))
is "total turns equals unique messages (no double-count)" "4" "$TOTAL"

# The pane's two percentages must sum to <= 100.
AEON_WIN="$(field SP_TOK_AEON_WIN)"
SESS_WIN="$(field SP_TOK_SESS_WIN)"
TOK_WIN="$(field SP_TOK_WIN)"
if [ "$TOK_WIN" -gt 0 ]; then
    AEON_PCT=$(( AEON_WIN * 100 / TOK_WIN ))
    SESS_PCT=$(( SESS_WIN * 100 / TOK_WIN ))
    SUM_PCT=$(( AEON_PCT + SESS_PCT ))
    if [ "$SUM_PCT" -le 100 ]; then
        ok "aeon% + session% <= 100 ($AEON_PCT + $SESS_PCT = $SUM_PCT)"
    else
        bad "aeon% + session% <= 100" "got $SUM_PCT"
    fi
fi

# Report mode prints the same split.
RPT="$(run_tokens report)"
SHARE="$(echo "$RPT" | sed -n 's/.*session share of all tokens: \([0-9]*\)%.*/\1/p')"
is "report: session share is 25%" "25" "$SHARE"

# ---------------------------------------------------------------------------
# Positive control: rename the worktree dir so it does not match the prefix.
# The turns that were attributed to aeons now land in session
# (law-absence-needs-a-positive-control).
# ---------------------------------------------------------------------------
echo ""
echo "positive control — worktree transcript in a non-matching directory"
PROJ2="$TMP/projects2"
mkdir -p "$PROJ2/-some-other-project" "$PROJ2/$SESS_DIR_NAME"
cp "$PROJ/$WT_DIR_NAME/transcript.jsonl" "$PROJ2/-some-other-project/"
cp "$PROJ/$SESS_DIR_NAME/session.jsonl"  "$PROJ2/$SESS_DIR_NAME/"

ENV2="$(env -i PATH="$PATH" HOME="$TMP" \
    SPIRA_RUN="$RUN" SPIRA_TOKEN_PROJECTS="$PROJ2" \
    SPIRA_TOKEN_WINDOW_H=87600 SPIRA_CONF="$TMP/no.conf" \
    bash "$HERE/tokens.sh" env 2>/dev/null)"
field2() { sed -n "s/^$1=//p" <<< "$ENV2"; }

is "control: aeon turns from log only"   "2" "$(field2 SP_TOK_AEON_TURNS)"
is "control: session turns get the rest" "2" "$(field2 SP_TOK_SESS_TURNS)"

# ---------------------------------------------------------------------------
echo ""
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
