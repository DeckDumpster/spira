#!/usr/bin/env bash
# bd-unacked-comment-deliver.sh — PostToolUse hook: deliver post-claim bead comments.
#
# After each tool call, finds comments on BEAD_ID that were added after the bead
# was claimed (started_at) by parties other than BEADS_ACTOR (the current aeon).
# Each comment is delivered once as additionalContext. Tracks delivered ids in
# $SPIRA_RUN/aeon-$BEAD_ID-comment-delivered.
#
# The aeon acknowledges each delivered comment with:
#   bd note $BEAD_ID "ACK <comment-id>: <how it was applied or why not>"
# bd-close-unacked-guard.sh refuses a close while any are unacknowledged.
set -uo pipefail

[ -t 0 ] || cat >/dev/null 2>&1 || true

BID="${BEAD_ID:-}"; ACTOR="${BEADS_ACTOR:-}"
DB="${SPIRA_DB:-}"; RUN="${SPIRA_RUN:-}"; BD="${SPIRA_BD:-bd}"
[ -n "$BID" ] && [ -n "$ACTOR" ] && [ -n "$DB" ] && [ -n "$RUN" ] || exit 0

out="$(BEAD_ID="$BID" BEADS_ACTOR="$ACTOR" SPIRA_BD="$BD" SPIRA_DB="$DB" \
       SPIRA_RUN="$RUN" python3 -c '
import json, os, subprocess, sys

bid   = os.environ["BEAD_ID"]
actor = os.environ["BEADS_ACTOR"]
bd    = os.environ["SPIRA_BD"]
db    = os.environ["SPIRA_DB"]
run   = os.environ["SPIRA_RUN"]
state = os.path.join(run, "aeon-" + bid + "-comment-delivered")

started_at = ""
delivered  = set()
if os.path.exists(state):
    lines = open(state).read().splitlines()
    started_at = lines[0] if lines else ""
    delivered  = set(lines[1:])

if not started_at:
    try:
        r = subprocess.run([bd, "-C", db, "show", bid, "--format", "json"],
                           capture_output=True, text=True, timeout=30)
        if r.returncode == 0 and r.stdout.strip():
            d = json.loads(r.stdout)
            d = d[0] if isinstance(d, list) else d
            started_at = d.get("started_at", "")
    except Exception:
        pass

if not started_at:
    sys.exit(0)

if not os.path.exists(state):
    try:
        open(state, "w").write(started_at + "\n")
    except Exception:
        pass

try:
    r = subprocess.run([bd, "-C", db, "comments", bid, "--json"],
                       capture_output=True, text=True, timeout=30)
    if r.returncode != 0 or not r.stdout.strip():
        sys.exit(0)
    comments = json.loads(r.stdout)
    if not isinstance(comments, list):
        comments = [comments]
except Exception:
    sys.exit(0)

new_ids   = []
new_parts = []
for c in comments:
    cid = c.get("id", "")
    if not cid:
        continue
    if c.get("author", "") == actor:
        continue
    if (c.get("created_at", "") or "") <= started_at:
        continue
    if cid in delivered:
        continue
    new_ids.append(cid)
    new_parts.append(
        "Post-claim comment on " + bid + " (id: " + cid + ", from: " +
        c.get("author", "?") + ") — acknowledge with:\n"
        "  bd note " + bid + " \"ACK " + cid + ": <how applied or why not>\"\n\n" +
        c.get("text", "")
    )

if not new_parts:
    sys.exit(0)

try:
    with open(state, "a") as f:
        for cid in new_ids:
            f.write(cid + "\n")
except Exception:
    pass

print("\n\n---\n\n".join(new_parts))
' 2>/dev/null)"

[ -n "$out" ] || exit 0
python3 -c 'import json,sys; print(json.dumps({"additionalContext": sys.argv[1]}))' "$out"
