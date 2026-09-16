#!/usr/bin/env bash
#
# test-tokens.sh — the token split attributes worktree and archivist transcripts correctly.
#
#   ./test-tokens.sh
#
# Verifies three-bucket attribution (aeons / archivist / session), dedupe across aeon
# logs and worktree transcripts, and the split the pane renders.
#
# No database, no network, under a second.
#
# defect: sp-rcr sp-d0jp
# covers: spira/tokens.sh
# scar: worktree transcripts were attributed to the interactive session; archivist spend landed in session because it runs under $SPIRA_RUN but not under the worktree prefix.
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
ARC_DIR_NAME="$(echo "$RUN/archivist/cwd" | sed 's|[/.]|-|g')"
SESS_DIR_NAME="-home-user-brain"
mkdir -p "$PROJ/$WT_DIR_NAME" "$PROJ/$ARC_DIR_NAME" "$PROJ/$SESS_DIR_NAME"

# ---------------------------------------------------------------------------
# Fixture: four inputs, five unique message ids
#
#   aeon log:              msg_shared, msg_aeon_only
#   worktree transcript:   msg_shared, msg_wt_only
#   archivist transcript:  msg_archivist
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

cat > "$PROJ/$ARC_DIR_NAME/arc.jsonl" <<ARCJSONL
{"type":"assistant","message":{"id":"msg_archivist","usage":{"input_tokens":100,"cache_creation_input_tokens":200,"cache_read_input_tokens":300,"output_tokens":50}},"timestamp":"$NOW"}
ARCJSONL

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
# under aeons. Five unique turns across four inputs: 3 aeon, 1 archivist, 1 session.
is "aeon turns (log + worktree, deduped)"  "3" "$(field SP_TOK_AEON_TURNS)"
is "archivist turns"                       "1" "$(field SP_TOK_ARC_TURNS)"
is "session turns (positive test)"         "1" "$(field SP_TOK_SESS_TURNS)"

# No turn in both: aeon + archivist + session equals the five unique messages.
TOTAL=$(( $(field SP_TOK_AEON_TURNS) + $(field SP_TOK_ARC_TURNS) + $(field SP_TOK_SESS_TURNS) ))
is "total turns equals unique messages (no double-count)" "5" "$TOTAL"

# The three pane percentages must sum to <= 100.
AEON_WIN="$(field SP_TOK_AEON_WIN)"
ARC_WIN="$(field SP_TOK_ARC_WIN)"
SESS_WIN="$(field SP_TOK_SESS_WIN)"
TOK_WIN="$(field SP_TOK_WIN)"
if [ "$TOK_WIN" -gt 0 ]; then
    AEON_PCT=$(( AEON_WIN * 100 / TOK_WIN ))
    ARC_PCT=$(( ARC_WIN * 100 / TOK_WIN ))
    SESS_PCT=$(( SESS_WIN * 100 / TOK_WIN ))
    SUM_PCT=$(( AEON_PCT + ARC_PCT + SESS_PCT ))
    if [ "$SUM_PCT" -le 100 ]; then
        ok "aeon% + archivist% + session% <= 100 ($AEON_PCT + $ARC_PCT + $SESS_PCT = $SUM_PCT)"
    else
        bad "aeon% + archivist% + session% <= 100" "got $SUM_PCT"
    fi
fi

# Report mode prints the same split.
RPT="$(run_tokens report)"
SHARE="$(echo "$RPT" | sed -n 's/.*session share of all tokens: \([0-9]*\)%.*/\1/p')"
is "report: session share is 20%" "20" "$SHARE"

# ---------------------------------------------------------------------------
# Positive control 1: rename the worktree dir so it does not match the prefix.
# The worktree turns that were attributed to aeons now land in session.
# Archivist is still in its correct dir (law-absence-needs-a-positive-control).
# ---------------------------------------------------------------------------
echo ""
echo "positive control 1 — worktree transcript in a non-matching directory"
PROJ2="$TMP/projects2"
mkdir -p "$PROJ2/-some-other-project" "$PROJ2/$ARC_DIR_NAME" "$PROJ2/$SESS_DIR_NAME"
cp "$PROJ/$WT_DIR_NAME/transcript.jsonl" "$PROJ2/-some-other-project/"
cp "$PROJ/$ARC_DIR_NAME/arc.jsonl"       "$PROJ2/$ARC_DIR_NAME/"
cp "$PROJ/$SESS_DIR_NAME/session.jsonl"  "$PROJ2/$SESS_DIR_NAME/"

ENV2="$(env -i PATH="$PATH" HOME="$TMP" \
    SPIRA_RUN="$RUN" SPIRA_TOKEN_PROJECTS="$PROJ2" \
    SPIRA_TOKEN_WINDOW_H=87600 SPIRA_CONF="$TMP/no.conf" \
    bash "$HERE/tokens.sh" env 2>/dev/null)"
field2() { sed -n "s/^$1=//p" <<< "$ENV2"; }

is "control1: aeon turns from log only"       "2" "$(field2 SP_TOK_AEON_TURNS)"
is "control1: archivist turns still correct"  "1" "$(field2 SP_TOK_ARC_TURNS)"
is "control1: session turns get the rest"     "2" "$(field2 SP_TOK_SESS_TURNS)"

# ---------------------------------------------------------------------------
# Positive control 2: place the archivist transcript in a non-matching directory.
# Its turns must land in session rather than archivist.
# ---------------------------------------------------------------------------
echo ""
echo "positive control 2 — archivist transcript in a non-matching directory"
PROJ3="$TMP/projects3"
mkdir -p "$PROJ3/$WT_DIR_NAME" "$PROJ3/-wrong-arc-dir" "$PROJ3/$SESS_DIR_NAME"
cp "$PROJ/$WT_DIR_NAME/transcript.jsonl" "$PROJ3/$WT_DIR_NAME/"
cp "$PROJ/$ARC_DIR_NAME/arc.jsonl"       "$PROJ3/-wrong-arc-dir/"
cp "$PROJ/$SESS_DIR_NAME/session.jsonl"  "$PROJ3/$SESS_DIR_NAME/"

ENV3="$(env -i PATH="$PATH" HOME="$TMP" \
    SPIRA_RUN="$RUN" SPIRA_TOKEN_PROJECTS="$PROJ3" \
    SPIRA_TOKEN_WINDOW_H=87600 SPIRA_CONF="$TMP/no.conf" \
    bash "$HERE/tokens.sh" env 2>/dev/null)"
field3() { sed -n "s/^$1=//p" <<< "$ENV3"; }

is "control2: archivist turns now zero"    "0" "$(field3 SP_TOK_ARC_TURNS)"
is "control2: session gets misplaced arc"  "2" "$(field3 SP_TOK_SESS_TURNS)"

# ---------------------------------------------------------------------------
echo ""
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
