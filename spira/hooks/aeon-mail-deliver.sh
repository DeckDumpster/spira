#!/usr/bin/env bash
# aeon-mail-deliver.sh — PostToolUse hook: deliver queued aeon mail as additionalContext.
#
# After each tool call, moves new messages from the aeon's Maildir inbox to cur/ and
# returns them as additionalContext. Cheap when empty (one readdir). The mailbox path
# and bead id come from the environment the aeon session inherits from aeon.sh.
set -uo pipefail

# Drain hook payload — not used but must be consumed to avoid blocking the client.
[ -t 0 ] || cat >/dev/null 2>&1 || true

MAIL="${SPIRA_MAIL:-}"; BID="${BEAD_ID:-}"
[ -n "$MAIL" ] && [ -n "$BID" ] || exit 0

NEW="$MAIL/aeon-$BID/new"
CUR="$MAIL/aeon-$BID/cur"
[ -d "$NEW" ] || exit 0

out=""
for f in "$NEW"/*; do
    [ -f "$f" ] || continue
    sender="$(sed -n 's/^From:[[:space:]]*//p' "$f" | head -1)"
    body="$(awk '/^[[:space:]]*$/{found=1;next} found{print}' "$f")"
    [ -n "$out" ] && out="$out"$'\n\n---\n\n'
    out="${out}Message from ${sender:-the harness} while you were working — re-read the bead if it changes scope"$'\n\n'"$body"
    mv "$f" "$CUR/$(basename "$f")" 2>/dev/null || true
done

[ -n "$out" ] || exit 0
python3 -c 'import json,sys; print(json.dumps({"additionalContext": sys.argv[1]}))' "$out"
