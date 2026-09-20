#!/usr/bin/env bash
# bd-close-unacked-guard.sh — PreToolUse hook: refuse bd close while the bead has
#   post-claim comments that the current aeon has not acknowledged.
#
# An aeon acknowledges a delivered comment with:
#   bd note $BEAD_ID "ACK <comment-id>: <how it was applied or why not>"
#
# The guard reads the bead's notes field for ACK <uuid>: patterns and checks each
# post-claim comment (added after started_at, not by BEADS_ACTOR) against them.
#
# Override (when unacknowledged comments are deliberately set aside):
#   SPIRA_CLOSE_UNACKED_CONSIDERED=<reason>
#
# Exit 2 BLOCKS the tool call and feeds stderr back to the model. Exit 0 allows.
set -uo pipefail

PAYLOAD="$(cat)"

[ -n "${SPIRA_AEON:-}" ] || exit 0

BID="${BEAD_ID:-}"; ACTOR="${BEADS_ACTOR:-}"
DB="${SPIRA_DB:-}"; BD="${SPIRA_BD:-bd}"
[ -n "$BID" ] && [ -n "$DB" ] || exit 0

HIT="$(GUARD_PAYLOAD="$PAYLOAD" BEAD_ID="$BID" BEADS_ACTOR="$ACTOR" \
       SPIRA_BD="$BD" SPIRA_DB="$DB" python3 -c '
import json, os, re, subprocess, sys

payload = os.environ.get("GUARD_PAYLOAD", "")
try:
    d = json.loads(payload)
except Exception:
    sys.exit(0)

if d.get("tool_name") != "Bash":
    sys.exit(0)

cmd = (d.get("tool_input") or {}).get("command", "")
if not cmd:
    sys.exit(0)

cmd_nosq = re.sub(r"\x27[^\x27]*\x27", " ", cmd)
segs = re.split(r"\|\||\&\&|[;|\n]", cmd_nosq)
found_close = False
for seg in segs:
    if re.search(r"(?:^|\s)(?:\w+=\S+\s+)*(?:[-\w./]*/)?bd\b[^\n]*\bclose\b", seg):
        found_close = True
        break
if not found_close:
    sys.exit(0)

bid   = os.environ.get("BEAD_ID", "")
actor = os.environ.get("BEADS_ACTOR", "")
bd    = os.environ.get("SPIRA_BD", "bd")
db    = os.environ.get("SPIRA_DB", "")
if not bid or not db:
    sys.exit(0)

try:
    r = subprocess.run([bd, "-C", db, "show", bid, "--format", "json"],
                       capture_output=True, text=True, timeout=30)
    if r.returncode != 0 or not r.stdout.strip():
        sys.exit(0)
    bead = json.loads(r.stdout)
    bead = bead[0] if isinstance(bead, list) else bead
    started_at = bead.get("started_at", "")
    notes = bead.get("notes", "") or ""
except Exception:
    sys.exit(0)

if not started_at:
    sys.exit(0)

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

# ACKs written by the aeon via: bd note $BID "ACK <uuid>: <explanation>"
acked = set(re.findall(
    r"ACK\s+([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})\s*:",
    notes
))

unacked = []
for c in comments:
    cid = c.get("id", "")
    if not cid:
        continue
    if c.get("author", "") == actor:
        continue
    if (c.get("created_at", "") or "") <= started_at:
        continue
    if cid in acked:
        continue
    text = (c.get("text", "") or "").replace("\n", " ")[:80]
    unacked.append(cid + "|" + c.get("author", "?") + "|" + text)

for entry in unacked:
    print(entry)
' 2>/dev/null)"

[ -n "$HIT" ] || exit 0

if [ -n "${SPIRA_CLOSE_UNACKED_CONSIDERED:-}" ]; then exit 0; fi
if printf '%s' "$PAYLOAD" | python3 -c '
import json,sys
d=json.load(sys.stdin)
c=(d.get("tool_input") or {}).get("command","")
sys.exit(0 if "SPIRA_CLOSE_UNACKED_CONSIDERED=" in c else 1)' 2>/dev/null; then exit 0; fi

printf '\nBLOCKED by bd-close-unacked-guard: the bead has post-claim comments that have not been acknowledged.\n\n' >&2
printf 'Acknowledge each with:\n' >&2
printf '  bd note %s "ACK <comment-id>: <how it was applied or why not>"\n\n' "$BID" >&2
printf 'Unacknowledged:\n' >&2
while IFS= read -r line; do
    [ -n "$line" ] || continue
    cid="${line%%|*}"; rest="${line#*|}"; author="${rest%%|*}"; text="${rest#*|}"
    printf '  %s (from: %s): %s\n' "$cid" "$author" "$text" >&2
done <<< "$HIT"
printf '\nOverride when unacknowledged comments are deliberately set aside:\n' >&2
printf '  SPIRA_CLOSE_UNACKED_CONSIDERED=<reason>\n' >&2
exit 2
