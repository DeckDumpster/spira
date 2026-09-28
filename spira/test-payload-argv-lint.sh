#!/usr/bin/env bash
#
# test-payload-argv-lint.sh — the fence that refuses a JSON payload handed to
# python3/jq/awk through argv or an environment variable (law-payloads-go-on-stdin).
#
#   ./test-payload-argv-lint.sh
#
# WHAT THIS SUITE IS FOR
# -----------------------
# Five outages in seventeen days, the last (sp-o4trx) an hour of 572 idle summons against
# 142 ready beads: every one was the same shape, fixed only at its own call site, because
# nothing ever scanned for the pattern. This is that scan, a T0 fence, and this suite is its
# positive control (law-absence-needs-a-positive-control): a bad shape is planted, the fence
# is required to name its line, the plant is withdrawn, and only then is the shipped tree's
# silence evidence of anything.
#
# THE FIXED SHAPE MUST STAY SILENT, not just the never-broken one. Every real occurrence in
# this tree was fixed by moving the payload to stdin or a temp file — a fence that cannot
# tell that fix from the defect it replaces would refuse this repository's own history.
#
# tier: T0
# covers: spira/payload-argv-lint.sh spira/gate-fences.sh spira/gate-spira.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
PROBE="$TMP/probe.sh"

# ---------------------------------------------------------------------------------------
# THE POSITIVE CONTROL — both offending shapes, planted in one file.
# ---------------------------------------------------------------------------------------
cat > "$PROBE" <<'EOF'
#!/usr/bin/env bash
epic_rank_rows() {
    local ready_json="$1" lookup_json="$2"
    python3 -c '
import json, sys
ready = json.loads(sys.argv[1])
print(ready)
' "$ready_json" "$lookup_json"
}
mark_queue_waiters() {
    LABELED_JSON="${labeled_json:-[]}" python3 -c '
import os
print(os.environ["LABELED_JSON"])
'
}
EOF

out="$(bash "$HERE/payload-argv-lint.sh" --scan "$PROBE")"
want "SEEN RED: the ARGV shape is caught, naming the closing line" '"$ready_json" "$lookup_json"' "$out"
want "and the ENV shape is caught, naming the assignment line"     "LABELED_JSON="              "$out"

# ---------------------------------------------------------------------------------------
# THE FIX IS SILENT. Every real defect in this tree was fixed by moving the same payload to
# stdin (or a temp file whose PATH — not content — travels as the argv/env token). A fence
# that flagged the fix would refuse this repository's own history.
# ---------------------------------------------------------------------------------------
cat > "$PROBE" <<'EOF'
#!/usr/bin/env bash
epic_rank_rows() {
    local ready_json="$1" lookup_json="$2"
    local _lkf; _lkf="$(mktemp)"
    printf '%s' "$lookup_json" > "$_lkf"
    printf '%s' "$ready_json" | LOOKUP_FILE="$_lkf" python3 -c '
import json, os, sys
ready = json.loads(sys.stdin.read())
with open(os.environ["LOOKUP_FILE"]) as f:
    lookup = json.load(f)
print(ready, lookup)
' "$resume_csv"
    rm -f "$_lkf"
}
report() {
    # A printf argument list that merely NAMES python3 and a *_json var elsewhere on the
    # line, with no inline script of its own, must not be mistaken for an invocation.
    printf 'fmt: %s %s\n' \
        "$(some_helper "$x")" \
        "$uc_json"
}
EOF
out="$(bash "$HERE/payload-argv-lint.sh" --scan "$PROBE")"
is "GREEN: stdin/temp-file payloads and an unrelated printf continuation are both silent" "" "$out"

# ---------------------------------------------------------------------------------------
# A SHORT, BOUNDED ARGV/ENV VARIABLE IS NOT THE DEFECT. started_csv/resume_csv-shaped names
# (no "json" substring) must never trip the fence merely for sitting on a closing-quote line
# or beside python3.
# ---------------------------------------------------------------------------------------
cat > "$PROBE" <<'EOF'
#!/usr/bin/env bash
epic_parent_lookup() {
    python3 -c '
import sys
print(sys.argv[1])
' "$started_csv"
}
EOF
out="$(bash "$HERE/payload-argv-lint.sh" --scan "$PROBE")"
is "GREEN: a short non-json-named argv token is silent" "" "$out"

# ---------------------------------------------------------------------------------------
# GATE INTEGRATION. A fence nothing invokes is a file.
# ---------------------------------------------------------------------------------------
want "gate-fences.sh lists this fence" "spira/payload-argv-lint.sh" "$(cat "$HERE/gate-fences.sh")"
want "gate-spira.sh calls this fence"  "spira/payload-argv-lint.sh" "$(cat "$HERE/gate-spira.sh")"
is   "and it is executable"           "0" "$([ -x "$HERE/payload-argv-lint.sh" ]; echo $?)"

# ---------------------------------------------------------------------------------------
# THE SHIPPED TREE. Read through the control above, this now means something.
#
# In the gate container a worktree's .git FILE resolves to the host, which is not
# bind-mounted inside the container: git exits non-zero and payload-argv-lint.sh exits 3.
# Build a portable mirror from the real files and run against that instead — the set of
# files is identical; only the git plumbing differs (same seam as test-bd-stdin.sh).
# ---------------------------------------------------------------------------------------
out="$(bash "$HERE/payload-argv-lint.sh")"; rc=$?
if [ "$rc" = 3 ]; then
    MIRROR="$TMP/shipped-mirror"
    mkdir -p "$MIRROR"
    SHIPPED="$(cd "$HERE/.." && pwd -P)"
    cp -a "$SHIPPED/." "$MIRROR/"
    rm -rf "$MIRROR/.git"
    git init -q -b main "$MIRROR"
    git -C "$MIRROR" config user.email t@t
    git -C "$MIRROR" config user.name t
    git -C "$MIRROR" add .
    git -C "$MIRROR" commit -q -m mirror
    out="$(bash "$MIRROR/spira/payload-argv-lint.sh")"; rc=$?
fi
is "the shipped tree passes" "0" "$rc"
[ "$rc" = 0 ] || printf '%s\n' "$out"

tl_summary
