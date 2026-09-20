#!/usr/bin/env bash
#
# bd-update-inflight-guard.sh — PreToolUse fence against editing spec fields on
#   a bead that is in flight, in concierge and interactive sessions.
#
#   bash bd-update-inflight-guard.sh    reads Claude Code's PreToolUse JSON on stdin
#
# Exit 2 BLOCKS the tool call and feeds stderr back to the model. Exit 0 allows.
#
# WHY THIS EXISTS
# ---------------
# `bd update <id> --description|--design|--acceptance|--body-file` rewrites a
# bead's specification with no check on whether an aeon holds it. The aeon read
# the bead at claim time and never sees the edit; it ships the old spec and opens
# a PR against it. Every downstream signal looks correct — the PR names the bead,
# the branch matches, the commit names the bead — so the divergence is invisible
# until a reviewer compares the PR against the bead and concludes the aeon ignored
# its brief.
#
# Three PRs were produced this way in one session; two had to be closed unmerged.
#
# In-flight evidence (any one is sufficient to block):
#   - status = in_progress
#   - non-empty assignee
#   - a branch:* label
#
# WHY CONCIERGE/INTERACTIVE SESSIONS ONLY
# ----------------------------------------
# Aeons legitimately update status, assignee and labels on their own beads; those
# are not spec fields. The party that rewrites a description while an aeon works
# is the concierge/operator path. Aeon sessions carry SPIRA_AEON; the guard exits
# 0 there, binding only the actor who produced the incident
# (law-guard-binds-the-caller).
#
# A fence is a polite refusal — the override is named and honoured.
#
# WIRING: register as a Claude Code PreToolUse hook in the operator's settings:
#   { "matcher": "Bash", "hooks": [{ "type": "command",
#     "command": "bash /path/to/spira/bd-update-inflight-guard.sh" }] }
# Source harness.sh first when the path must be derived from SPIRA_HOME:
#   "command": "bash -c '. .claude/harness.sh 2>/dev/null; bash \"$SPIRA_HOME/bd-update-inflight-guard.sh\"'"
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"

PAYLOAD="$(cat)"

# CONCIERGE/INTERACTIVE SESSIONS ONLY. Aeons carry SPIRA_AEON; their status and
# label updates on their own beads are not spec edits. Binding this guard to aeon
# sessions would fence the wrong caller (law-guard-binds-the-caller).
[ -z "${SPIRA_AEON:-}" ] || exit 0

# The Python block parses the command, looks up the bead, and returns one of:
#   INFLIGHT:<id>:<evidence>   — spec-field edit targeting an in-flight bead
#   (nothing, exits 0)         — allow the call
HIT="$(GUARD_PAYLOAD="$PAYLOAD" \
  SPIRA_HOME="${SPIRA_HOME:-$HERE}" \
  SPIRA_DB="${SPIRA_DB:-}" \
  SPIRA_BD="${SPIRA_BD:-}" \
  python3 << 'PYEOF'
import json, os, re, subprocess, sys

payload = os.environ.get("GUARD_PAYLOAD", "")
try:
    d = json.loads(payload)
except Exception:
    sys.exit(0)

if d.get("tool_name") != "Bash":
    sys.exit(0)

c = (d.get("tool_input") or {}).get("command", "")
if not c:
    sys.exit(0)

# Strip heredoc bodies: content passed via heredoc is data, not an invocation.
def drop_heredocs(s):
    for m in list(re.finditer(r"<<-?\s*[\x27\"]?(\w+)[\x27\"]?", s)):
        tag = m.group(1)
        end = re.search(r"^\s*" + re.escape(tag) + r"\s*$", s[m.end():], re.M)
        if end:
            s = s[:m.end()] + s[m.end() + end.end():]
    return s

c = drop_heredocs(c)

# Strip single-quoted spans (prose documentation must not fire the guard).
c_stripped = re.sub(r"\x27[^\x27]*\x27", " ", c)

# Spec fields: flags that replace or amend a bead's description, design, or acceptance.
SPEC_FLAGS = frozenset({
    "--description", "-d",
    "--design", "--design-file",
    "--acceptance",
    "--body-file",
})

segs = re.split(r"\|\||\&\&|[;|\n]", c_stripped)

for seg in segs:
    # Match: [env=val ...] [path/]bd [-C <db>] update [...]
    if not re.search(
        r"(?:^|\s)(?:\w+=\S+\s+)*(?:[-\w./]*/)?bd\b[^\n]*\bupdate\b",
        seg
    ):
        continue

    # Extract everything after the last 'update' keyword in this segment.
    m = re.search(r"\bupdate\b(.*)", seg, re.DOTALL)
    if not m:
        continue
    rest = m.group(1)

    # Does this update touch a spec field?
    has_spec = False
    for flag in SPEC_FLAGS:
        if re.search(r"(?<!\w)" + re.escape(flag) + r"(?=\s|=|$)", rest):
            has_spec = True
            break
    if not has_spec:
        continue

    # Extract the bead ID: first token that matches the bead id pattern
    # and is not preceded by a flag (i.e., not a flag value).
    # Walk tokens in rest; skip flag tokens (start with -) and their values.
    bead_id = None
    tokens = rest.split()
    i = 0
    while i < len(tokens):
        tok = tokens[i]
        if tok.startswith("-"):
            # A flag; skip it and its value (unless value looks like a bead id
            # or starts with - itself).
            if "=" not in tok and i + 1 < len(tokens) and not tokens[i + 1].startswith("-"):
                i += 2  # skip flag and its value
            else:
                i += 1  # skip flag only (value was inline via = or no value)
        else:
            if re.match(r"^[a-z]{2}-[a-z0-9]+$", tok):
                bead_id = tok
                break
            i += 1

    if not bead_id:
        continue  # No bead id visible; allow rather than over-block.

    # Determine which database to query.
    # Prefer an explicit -C <path> in the command; fall back to SPIRA_DB.
    db = os.environ.get("SPIRA_DB", "")
    c_flag = re.search(
        r"(?:^|\s)(?:[-\w./]*/)?bd\s+-C\s+(\S+)",
        seg
    )
    if c_flag:
        db = c_flag.group(1).strip("\"'")

    if not db:
        continue  # No database path; allow rather than over-block.

    bd_bin = os.environ.get("SPIRA_BD", "") or "bd"

    try:
        proc = subprocess.run(
            [bd_bin, "-C", db, "show", bead_id, "--format", "json"],
            capture_output=True, text=True, timeout=5
        )
        if proc.returncode != 0 or not proc.stdout.strip():
            continue  # Bead not found; allow.

        raw = json.loads(proc.stdout)
        bead = raw[0] if isinstance(raw, list) else raw
        if not isinstance(bead, dict):
            continue

        status   = bead.get("status", "") or ""
        assignee = bead.get("assignee", "") or ""
        raw_labels = bead.get("labels") or []
        if isinstance(raw_labels, str):
            raw_labels = raw_labels.split()

        branch_label = next(
            (l for l in raw_labels if l and l.startswith("branch:")), None
        )

        evidence = []
        if status == "in_progress":
            evidence.append("status=in_progress")
        if assignee.strip():
            evidence.append("assignee=" + assignee.strip())
        if branch_label:
            evidence.append(str(branch_label))

        if evidence:
            print("INFLIGHT:" + bead_id + ":" + ",".join(evidence))
            sys.exit(0)

    except Exception:
        pass  # Fail open on any error so a lookup failure never blocks work.

PYEOF
)"

[ -n "$HIT" ] || exit 0

# OVERRIDE, named here and honoured. A deliberate mid-flight rewrite — a design
# change that cannot wait — may set BEAD_EDIT_IN_FLIGHT_CONSIDERED=1. The flag
# appears in the transcript rather than in a config the model sets once and forgets.
[ "${BEAD_EDIT_IN_FLIGHT_CONSIDERED:-0}" = "1" ] && exit 0
if printf '%s' "$PAYLOAD" | python3 -c '
import json, sys
d = json.load(sys.stdin)
c = (d.get("tool_input") or {}).get("command", "")
sys.exit(0 if "BEAD_EDIT_IN_FLIGHT_CONSIDERED=1" in c else 1)
' 2>/dev/null; then exit 0; fi

BEAD_ID="${HIT#INFLIGHT:}"
BEAD_ID="${BEAD_ID%%:*}"
EVIDENCE="${HIT#INFLIGHT:}"
EVIDENCE="${EVIDENCE#*:}"

printf '\nBLOCKED by bd-update-inflight-guard: bead %s is in flight.\n\n' "$BEAD_ID" >&2
printf 'Evidence: %s\n\n' "$EVIDENCE" >&2
printf 'Rewriting a spec field while an aeon works the bead is invisible to it.\n' >&2
printf 'The aeon ships the old spec; every downstream signal looks correct.\n' >&2
printf 'Three PRs were produced this way in one session; two closed unmerged.\n\n' >&2
printf 'Safe actions:\n' >&2
printf '  Comment on the bead (visible to the next aeon, harmless to the current one):\n' >&2
printf '    bd -C "$SPIRA_DB" comment %s "<note>"\n' "$BEAD_ID" >&2
printf '  File a successor bead with the revised spec and link it:\n' >&2
printf '    bd -C "$SPIRA_DB" create --title "<revised title>" ...\n' >&2
printf '    bd -C "$SPIRA_DB" link %s <new-id> --type blocks\n\n' "$BEAD_ID" >&2
printf 'OVERRIDE for a deliberate mid-flight rewrite (shows up in transcript):\n' >&2
printf '  BEAD_EDIT_IN_FLIGHT_CONSIDERED=1\n' >&2
exit 2
