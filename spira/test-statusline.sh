#!/usr/bin/env bash
#
# test-statusline.sh — statusline-check.py classifies every status-line configuration shape.
#
#   ./test-statusline.sh
#
# WHAT IT HOLDS. The status line lives in the CLIENT's settings, outside every repository, so
# nothing that lands here can fix it — the right message is all that can be owed. This suite
# exercises statusline-check.py's classification of every shape it can be handed:
#
#   1. THE EXISTING FOUR VERDICTS ARE UNCHANGED — ours, other, absent, unreadable — along with
#      the boolean-refreshInterval edge case.
#   2. STALE: a different copy of ctx-meter.sh, discovered either directly in the command or one
#      level deep inside a wrapper it names.
#   3. FRAGILE: a command that lives in /tmp or in a worktree, where it will not survive.
#   4. THE RESOLUTION: the /tmp/sl2/rec.sh shape that caused this bead, where the command is a
#      wrapper under /tmp that pipes to a ctx-meter.sh copy in a worktree.
#
# defect: sp-9ydp
# tier: T1
# covers: spira/statusline-check.py
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

has() { case "$3" in *"$2"*) ok "$1" ;; *) bad "$1" "contains [$2]" "$3" ;; esac; }

echo "test-statusline.sh"

TMP="$(mktemp -d)"
TMP_METER_DIR="/tmp/test-statusline-$$"
WRAPPER_DIR="/tmp/test-statusline-wrapper-$$"
trap 'rm -rf "$TMP" "$TMP_METER_DIR" "$WRAPPER_DIR"' EXIT

# The meter that is LANDED, and the runtime directory whose worktree/ subdirectory is the
# concerning location.  Both are pinned to non-defaults so a literal in the code cannot pass.
METER_DIR="$TMP/harness/spira"
METER="$METER_DIR/ctx-meter.sh"
RUN="$TMP/runtime"
mkdir -p "$METER_DIR" "$RUN/worktree/sp-0uu/spira"
touch "$METER"

# check <tag> <json> -> the one-line output of statusline-check.py
check() {
    local settings="$TMP/settings-$1.json"
    printf '%s\n' "$2" > "$settings"
    python3 "$HERE/statusline-check.py" "$settings" "$METER" "$RUN"
}

# ---------------------------------------------------------------------------
# THE EXISTING FOUR VERDICTS, unchanged.
# ---------------------------------------------------------------------------
echo
echo "existing verdicts"

out="$(check ours-interval "{\"statusLine\":{\"command\":\"$METER\",\"refreshInterval\":5}}")"
is "ours with interval" "ours 5" "$out"

out="$(check ours-no-interval "{\"statusLine\":{\"command\":\"$METER\"}}")"
is "ours no interval" "ours -" "$out"

out="$(check ours-wrapped "{\"statusLine\":{\"command\":\"bash $METER --debug\",\"refreshInterval\":10}}")"
is "ours wrapped in interpreter" "ours 10" "$out"

out="$(check absent "{\"theme\":\"dark\"}")"
is "absent" "absent" "$out"

printf 'not json' > "$TMP/settings-unreadable.json"
out="$(python3 "$HERE/statusline-check.py" "$TMP/settings-unreadable.json" "$METER" "$RUN")"
has "unreadable" "unreadable" "$out"

out="$(check other "{\"statusLine\":{\"command\":\"date\",\"refreshInterval\":5}}")"
is "other" "other 5" "$out"

out="$(check other-no-interval "{\"statusLine\":{\"command\":\"date\"}}")"
is "other no interval" "other -" "$out"

out="$(check bool-interval "{\"statusLine\":{\"command\":\"$METER\",\"refreshInterval\":true}}")"
is "boolean interval treated as absent" "ours -" "$out"

# ---------------------------------------------------------------------------
# STALE: a different ctx-meter.sh.
# ---------------------------------------------------------------------------
echo
echo "stale verdicts"

# A ctx-meter.sh at a generic different location.
ELSEWHERE="$TMP/elsewhere/spira"
mkdir -p "$ELSEWHERE"
touch "$ELSEWHERE/ctx-meter.sh"
out="$(check stale-elsewhere "{\"statusLine\":{\"command\":\"$ELSEWHERE/ctx-meter.sh\",\"refreshInterval\":5}}")"
is "stale elsewhere" "stale $ELSEWHERE/ctx-meter.sh 5" "$out"

# A ctx-meter.sh under the worktree directory.
WT_METER="$RUN/worktree/sp-0uu/spira/ctx-meter.sh"
touch "$WT_METER"
out="$(check stale-worktree "{\"statusLine\":{\"command\":\"$WT_METER\",\"refreshInterval\":5}}")"
is "stale worktree" "stale $WT_METER 5" "$out"

# A ctx-meter.sh under /tmp.
mkdir -p "$TMP_METER_DIR"
touch "$TMP_METER_DIR/ctx-meter.sh"
out="$(check stale-tmp "{\"statusLine\":{\"command\":\"$TMP_METER_DIR/ctx-meter.sh\",\"refreshInterval\":5}}")"
is "stale under /tmp" "stale $TMP_METER_DIR/ctx-meter.sh 5" "$out"

# Stale preserves the interval, including the absent case.
out="$(check stale-no-interval "{\"statusLine\":{\"command\":\"$ELSEWHERE/ctx-meter.sh\"}}")"
is "stale no interval" "stale $ELSEWHERE/ctx-meter.sh -" "$out"

# ---------------------------------------------------------------------------
# THE /tmp/sl2/rec.sh SHAPE: a wrapper under /tmp that pipes to a worktree copy.
# This is the exact case from sp-9ydp that was misclassified as `other`.
# ---------------------------------------------------------------------------
echo
echo "the wrapper shape from sp-9ydp"

mkdir -p "$WRAPPER_DIR"
cat > "$WRAPPER_DIR/rec.sh" <<EOF
#!/bin/bash
# a temporary recorder that wraps the meter
exec $WT_METER "\$@"
EOF
out="$(check wrapper "{\"statusLine\":{\"command\":\"$WRAPPER_DIR/rec.sh\",\"refreshInterval\":5}}")"
is "wrapper resolves to stale worktree copy" "stale $WT_METER 5" "$out"

# A wrapper that does NOT reference ctx-meter.sh falls through to fragile (it is under /tmp).
cat > "$WRAPPER_DIR/other.sh" <<'EOF'
#!/bin/bash
echo "hello"
EOF
out="$(check wrapper-no-meter "{\"statusLine\":{\"command\":\"$WRAPPER_DIR/other.sh\",\"refreshInterval\":5}}")"
is "wrapper without meter is fragile" "fragile $WRAPPER_DIR/other.sh 5" "$out"

# ---------------------------------------------------------------------------
# FRAGILE: a command under /tmp or a worktree, but NOT ctx-meter.sh.
# ---------------------------------------------------------------------------
echo
echo "fragile verdicts"

touch "$TMP_METER_DIR/something-else.sh"
out="$(check fragile-tmp "{\"statusLine\":{\"command\":\"$TMP_METER_DIR/something-else.sh\",\"refreshInterval\":5}}")"
is "fragile under /tmp" "fragile $TMP_METER_DIR/something-else.sh 5" "$out"

WT_OTHER="$RUN/worktree/sp-0uu/spira/other-tool.sh"
touch "$WT_OTHER"
out="$(check fragile-worktree "{\"statusLine\":{\"command\":\"$WT_OTHER\",\"refreshInterval\":5}}")"
is "fragile under worktree" "fragile $WT_OTHER 5" "$out"

# ---------------------------------------------------------------------------
echo
tl_summary
