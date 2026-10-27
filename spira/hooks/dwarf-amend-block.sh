#!/usr/bin/env bash
# dwarf-amend-block.sh — PreToolUse hook: block every tool call while an amendment to this
# dwarf's bead is unanswered, except the `work ack` that answers it.
set -uo pipefail
payload="$(cat 2>/dev/null || true)"
MAIL="${SPIRA_MAIL:-}"; BID="${BEAD_ID:-}"
[ -n "$MAIL" ] && [ -n "$BID" ] || exit 0
[ -d "$MAIL/dwarf-$BID/amend" ] || exit 0

printf '%s' "$payload" | python3 -c '
import glob, json, os, re, sys
mail, bid = sys.argv[1], sys.argv[2]
pending = []
for f in sorted(glob.glob(os.path.join(mail, "dwarf-" + bid, "amend", "am-*.json"))):
    try:
        r = json.load(open(f))
    except Exception:
        continue
    if r.get("state") == "pending":
        pending.append(r)
if not pending:
    sys.exit(0)
try:
    p = json.load(sys.stdin)
except Exception:
    p = {}
cmd = (p.get("tool_input") or {}).get("command", "") if p.get("tool_name") == "Bash" else ""
ack = re.fullmatch(r"\s*work ack (am-[0-9a-f]+) (--accept|--decline (\"[^\"`$\\]+\"|\x27[^\x27]+\x27|[^\s;&|`$()<>\"\x27]+))\s*", cmd)
if ack and any(r["id"] == ack.group(1) for r in pending):
    sys.exit(0)
text = "\n\n---\n\n".join(
    "Amendment %s from %s must be answered before you do anything else.\n\n%s\n\nRun `work ack %s --accept` to take it on, or `work ack %s --decline \"<why>\"`. Unanswered by its deadline, or declined, the amender may slay this session and requeue the bead with the amendment merged in."
    % (r["id"], r.get("amender", "the amender"), r.get("text", "").strip(), r["id"], r["id"])
    for r in pending)
print(json.dumps({"decision": "block", "reason": text}))
' "$MAIL" "$BID"
exit 0
