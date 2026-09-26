#!/usr/bin/env bash
#
# test-ctx-pointer.sh — the operator pointer: env mode reports the operator's session, not the
# newest transcript on the box.
#
#   ./test-ctx-pointer.sh
#
# WHAT THIS SUITE IS GUARDING. The collector's CTX row must describe the operator's session.
# Without the pointer, env mode picks the newest-mtime transcript across every project, and
# with aeons running that is nearly always an aeon's worktree transcript. The status-line hook
# persists a pointer on every tick; env mode reads that pointer instead of scanning.
#
# THE FIXTURE HAS TWO TRANSCRIPTS: an operator's and a newer aeon's. The acceptance criterion
# from the bead is that env mode reports the operator's, not the aeon's, when the pointer is
# fresh — and reports `-` when the pointer is stale, never the aeon's.
#
# defect: sp-881
# tier: T1
# covers: spira/ctx-meter.sh
# scar: ctx-meter env mode picked the newest-mtime transcript across every project; with aeons running it reported an aeon's worktree transcript rather than the operator's session.
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
CTX="$HERE/ctx-meter.sh"

has() { case "$2" in *"$3"*) ok "$1" ;; *) bad "$1" "no [$3] in [$2]" ;; esac; }
hasnt() { case "$2" in *"$3"*) bad "$1" "found [$3] in [$2]" ;; *) ok "$1" ;; esac; }

T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT INT TERM
mkdir -p "$T/home" "$T/run" "$T/projects/-operator-project" "$T/projects/-aeon-worktree"

NONE="$T/no-such.conf"
CW=900; CH=1800; CL=2700
EPOCH=1750000000
IDLE=300

meter() {
    local mode="$1" at="$2"; shift 2
    env -i HOME="$T/home" PATH="$PATH" SPIRA_CONF="$NONE" \
        SPIRA_RUN="$T/run" SPIRA_TOKEN_PROJECTS="$T/projects" \
        SPIRA_CTX_WARN="$CW" SPIRA_CTX_HIGH="$CH" SPIRA_CTX_LIMIT="$CL" \
        SPIRA_NOW="$at" SPIRA_ARCHIVIST_IDLE="$IDLE" \
        bash "$CTX" "$mode" "$@" 2>/dev/null
}
val() { sed -n "s/^$1=//p"; }

blob() {
    local sid="$1" tp="$2" ctx="$3"
    python3 - "$sid" "$tp" "$ctx" <<'PY'
import json, sys
sid, tp, ctx = sys.argv[1], sys.argv[2], int(sys.argv[3])
print(json.dumps({
    "session_id": sid,
    "transcript_path": tp,
    "cwd": "/tmp",
    "context_window": {"total_input_tokens": ctx, "current_usage": {"input_tokens": ctx},
                       "used_percentage": 1, "context_window_size": 1000000},
}))
PY
}

# Two transcripts: operator at 115k/19t, aeon at 49k/1t. The aeon's is NEWER by mtime.
OP_TP="$T/projects/-operator-project/op-session.jsonl"
AEON_TP="$T/projects/-aeon-worktree/aeon-session.jsonl"

# The operator's transcript: 19 turns at 115k.
for i in $(seq 1 19); do
    printf '{"type":"assistant","message":{"id":"m-op-%d","usage":{"input_tokens":%d,"cache_creation_input_tokens":0,"cache_read_input_tokens":0,"output_tokens":10}}}\n' \
        "$i" "$((115000 / 19))" >> "$OP_TP"
done

# The aeon's transcript: 1 turn at 49k, written AFTER the operator's so its mtime is newer.
sleep 0.05
printf '{"type":"assistant","message":{"id":"m-aeon-1","usage":{"input_tokens":49000,"cache_creation_input_tokens":0,"cache_read_input_tokens":0,"output_tokens":10}}}\n' > "$AEON_TP"

PTR="$T/run/ctx-operator.ptr"

# ==========================================================================================
echo
echo "without the pointer, env reports nothing (not the newest)"
# ==========================================================================================
rm -f "$PTR"
E="$(meter env "$EPOCH")"
is "no pointer means no session" "-" "$(val SP_CTX_NOW <<<"$E")"
is "and SP_CTX_AGE says no pointer" "-" "$(val SP_CTX_AGE <<<"$E")"

# ==========================================================================================
echo
echo "the status-line hook writes the pointer"
# ==========================================================================================
blob "op-session" "$OP_TP" 115000 | meter line "$EPOCH"  >/dev/null
is "the pointer file exists" "yes" "$([ -f "$PTR" ] && echo yes || echo no)"
has "it names the operator transcript" "$(cat "$PTR")" "$OP_TP"
has "it carries the timestamp" "$(cat "$PTR")" "ts=$EPOCH"

# ==========================================================================================
echo
echo "env reads the pointer and reports the operator's session"
# ==========================================================================================
# The aeon transcript is newer by mtime, but the pointer says the operator's.
E="$(meter env "$EPOCH")"
# The operator has 19 turns.
is "env reports the operator's turn count" "19" "$(val SP_CTX_TURNS <<<"$E")"
is "SP_CTX_AGE is 0 (pointer just written)" "0" "$(val SP_CTX_AGE <<<"$E")"
hasnt "it does not report 49000 (the aeon)" "$E" "SP_CTX_NOW=49000"

# ==========================================================================================
echo
echo "a stale pointer falls back to no session"
# ==========================================================================================
# Advance the clock past the idle threshold.
E="$(meter env "$((EPOCH + IDLE + 1))")"
is "a stale pointer reports no session" "-" "$(val SP_CTX_NOW <<<"$E")"
# The age should reflect the pointer's age, not `-`.
is "SP_CTX_AGE reports the pointer age" "$((IDLE + 1))" "$(val SP_CTX_AGE <<<"$E")"

# ==========================================================================================
echo
echo "a refreshed pointer keeps it alive"
# ==========================================================================================
blob "op-session" "$OP_TP" 120000 | meter line "$((EPOCH + 200))" >/dev/null
E="$(meter env "$((EPOCH + 200))")"
is "a refreshed pointer reports the session" "19" "$(val SP_CTX_TURNS <<<"$E")"
is "SP_CTX_AGE is 0" "0" "$(val SP_CTX_AGE <<<"$E")"

# ==========================================================================================
echo
echo "a named transcript in env mode (the archivist) ignores the pointer"
# ==========================================================================================
E="$(meter env "$EPOCH" "$AEON_TP")"
is "a named transcript reports that transcript's turns" "1" "$(val SP_CTX_TURNS <<<"$E")"

# ==========================================================================================
echo
echo "pointer with a missing transcript falls back to no session"
# ==========================================================================================
rm -f "$PTR"
blob "op-session" "$OP_TP" 115000 | meter line "$EPOCH" >/dev/null
rm -f "$OP_TP"
E="$(meter env "$EPOCH")"
is "a pointer to a deleted transcript reports no session" "-" "$(val SP_CTX_NOW <<<"$E")"
# Restore for further tests.
for i in $(seq 1 19); do
    printf '{"type":"assistant","message":{"id":"m-op-%d","usage":{"input_tokens":%d,"cache_creation_input_tokens":0,"cache_read_input_tokens":0,"output_tokens":10}}}\n' \
        "$i" "$((115000 / 19))" >> "$OP_TP"
done

# ==========================================================================================
echo
echo "a corrupt pointer file falls back to no session"
# ==========================================================================================
printf 'this is garbage\n' > "$PTR"
E="$(meter env "$EPOCH")"
is "a corrupt pointer reports no session" "-" "$(val SP_CTX_NOW <<<"$E")"

# ==========================================================================================
echo
echo "a second interactive session's idle tick does not steal the pointer"
# ==========================================================================================
# THE CASE. The concierge is a second Claude client in the same project and its status line
# ticks on a TIMER, whether or not anyone is talking to it. Both sessions write this pointer.
# Before the fix the last writer won, so the idle session took the pointer between the
# operator's turns and the pane flipped to `-` and back on every pass. IDLE_TP stands in for
# the concierge: a real transcript that stopped growing.
IDLE_TP="$T/projects/-operator-project/idle-session.jsonl"
printf '{"type":"assistant","message":{"id":"m-idle-1","usage":{"input_tokens":9000,"cache_creation_input_tokens":0,"cache_read_input_tokens":0,"output_tokens":10}}}\n' > "$IDLE_TP"
sleep 0.05
# The operator speaks last, so his transcript is the newer one by mtime.
printf '{"type":"assistant","message":{"id":"m-op-20","usage":{"input_tokens":6000,"cache_creation_input_tokens":0,"cache_read_input_tokens":0,"output_tokens":10}}}\n' >> "$OP_TP"

rm -f "$PTR"
blob "op-session" "$OP_TP" 115000 | meter line "$EPOCH" >/dev/null
has "the operator holds the pointer" "$(cat "$PTR")" "$OP_TP"

# POSITIVE CONTROL FIRST: the idle session's tick must be capable of writing the pointer at
# all, or "it did not steal it" proves nothing. With no pointer present, it takes it.
rm -f "$PTR"
blob "idle-session" "$IDLE_TP" 9000 | meter line "$EPOCH" >/dev/null
has "POSITIVE CONTROL — the idle session can write the pointer" "$(cat "$PTR")" "$IDLE_TP"

# Now the real case: the operator holds it, and the idle session ticks.
rm -f "$PTR"
blob "op-session" "$OP_TP" 115000 | meter line "$EPOCH" >/dev/null
blob "idle-session" "$IDLE_TP" 9000 | meter line "$EPOCH" >/dev/null
has "an idle tick does not take the pointer from the operator" "$(cat "$PTR")" "$OP_TP"
hasnt "and the pointer does not name the idle session" "$(cat "$PTR")" "$IDLE_TP"
E="$(meter env "$EPOCH")"
[ "$(val SP_CTX_NOW <<<"$E")" != "-" ] \
    && ok "env still reports a live session" \
    || bad "env still reports a live session" "got [-]"

# THE HOLDER MUST NOT BE ABLE TO KEEP IT FOREVER. Once its tick is older than the idle
# threshold it has stopped speaking, and the next session to tick takes the pointer.
rm -f "$PTR"
blob "op-session" "$OP_TP" 115000 | meter line "$EPOCH" >/dev/null
blob "idle-session" "$IDLE_TP" 9000 | meter line "$((EPOCH + IDLE + 60))" >/dev/null
has "a stale holder yields the pointer" "$(cat "$PTR")" "$IDLE_TP"

# AND THE HOLDER MAY ALWAYS REFRESH ITS OWN. Yielding is about OTHER transcripts; a session
# writing the pointer it already holds must still advance its timestamp, or its own pointer
# goes stale while it is talking.
rm -f "$PTR"
blob "op-session" "$OP_TP" 115000 | meter line "$EPOCH" >/dev/null
blob "op-session" "$OP_TP" 115000 | meter line "$((EPOCH + 30))" >/dev/null
has "the holder refreshes its own timestamp" "$(cat "$PTR")" "ts=$((EPOCH + 30))"

echo
tl_summary
