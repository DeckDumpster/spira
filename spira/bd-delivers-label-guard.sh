#!/usr/bin/env bash
# bd-delivers-label-guard.sh — PreToolUse hook: refuse bd label remove <id> delivers:*
#   in aeon sessions.
#
#   bash bd-delivers-label-guard.sh    reads Claude Code's PreToolUse JSON on stdin
#
# Exit 2 BLOCKS the tool call and feeds stderr back to the model. Exit 0 allows.
#
# A delivers: label is a closing criterion stamped at filing time by the producer.
# Nothing in the harness removes it programmatically — incident.sh retire-unsatisfiable-delivers
# removes delivers:note: labels, but that subcommand is never called from aeon sessions. So
# any aeon that runs bd label remove on a delivers: label is escaping a criterion rather
# than satisfying it. The guard refuses and names the producer.
set -uo pipefail

[ -n "${SPIRA_AEON:-}" ] || exit 0

PAYLOAD="$(cat)"

result="$(printf '%s' "$PAYLOAD" | \
    SPIRA_BD="${SPIRA_BD:-bd}" \
    SPIRA_DB="${SPIRA_DB:-}" \
    BEAD_ID="${BEAD_ID:-}" \
    python3 -c '
import json, os, re, subprocess, sys

try:
    d = json.load(sys.stdin)
except Exception:
    sys.exit(0)

if d.get("tool_name") != "Bash":
    sys.exit(0)

cmd = (d.get("tool_input") or {}).get("command", "")
if not cmd:
    sys.exit(0)

# Strip heredoc bodies — data, not commands.
def drop_heredocs(s):
    for m in list(re.finditer(r"<<-?\s*[\x27\"]?(\w+)[\x27\"]?", s)):
        tag = m.group(1)
        end = re.search(r"^\s*" + re.escape(tag) + r"\s*$", s[m.end():], re.M)
        if end:
            s = s[:m.end()] + s[m.end() + end.end():]
    return s

cmd = drop_heredocs(cmd)
cmd = re.sub(r"\x27[^\x27]*\x27", " ", cmd)  # strip single-quoted spans

segs = re.split(r"\|\||\&\&|[;|\n]", cmd)

bid_hit = None
for seg in segs:
    # Match: bd [opts] label remove <id> delivers:*
    m = re.search(
        r"(?:^|\s)(?:\w+=\S+\s+)*(?:[-\w./]*/)?bd\b[^\n]*\blabel\b[^\n]*\bremove\b[^\n]*\bdelivers:",
        seg
    )
    if not m:
        continue
    # Extract the bead id: first positional after "remove" that is not a flag or delivers:
    toks = seg.split()
    try:
        ri = next(i for i, t in enumerate(toks) if t == "remove")
    except StopIteration:
        ri = -1
    if ri < 0:
        bid_hit = os.environ.get("BEAD_ID", "") or "unknown"
        break
    for t in toks[ri+1:]:
        if t.startswith("-"):
            continue
        if t.startswith("delivers:"):
            break
        bid_hit = t.strip("\"' ")
        break
    if bid_hit is None:
        bid_hit = os.environ.get("BEAD_ID", "") or "unknown"
    break

if bid_hit is None:
    sys.exit(0)

# Look up the producer.
bd  = os.environ.get("SPIRA_BD", "bd")
db  = os.environ.get("SPIRA_DB", "")
producer = ""
if db and bid_hit and bid_hit != "unknown":
    try:
        r = subprocess.run([bd, "-C", db, "show", bid_hit, "--format", "json"],
                           capture_output=True, text=True, timeout=10)
        if r.returncode == 0 and r.stdout.strip():
            b = json.loads(r.stdout)
            b = b[0] if isinstance(b, list) else b
            producer = b.get("created_by", "") or ""
    except Exception:
        pass

print("HIT:" + (producer or "unknown"))
' 2>/dev/null)"

case "$result" in HIT:*) ;; *) exit 0 ;; esac

producer="${result#HIT:}"

printf '\nBLOCKED by bd-delivers-label-guard: removes delivers: label — a closing criterion.\n\n' >&2
printf 'A delivers: label is stamped at filing time by the producer and records the\n' >&2
printf 'evidence required to close this bead. Removing it escapes the criterion rather\n' >&2
printf 'than satisfying it.\n\n' >&2
if [ -n "$producer" ] && [ "$producer" != "unknown" ]; then
    printf 'Producer: %s\n' "$producer" >&2
    printf '\nEscalate to %s if the criterion cannot be met.\n' "$producer" >&2
else
    printf 'Escalate to the bead producer if the criterion cannot be met.\n' >&2
fi
exit 2
