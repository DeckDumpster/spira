#!/usr/bin/env bash
#
# mail-fence.sh — refuse writes into SPIRA_MAIL except through mail.sh.
#
#   bash mail-fence.sh    reads Claude Code's PreToolUse JSON on stdin
#
# Exit 2 BLOCKS the tool call and feeds stderr back to the model. Exit 0 allows.
#
# WHY THIS EXISTS
# ---------------
# mail.sh send is the only sanctioned writer for mailboxes. It enforces atomic
# delivery (write to tmp/, rename to new/) and lint rules that ensure every
# message names a sender, carries a subject, and meets its kind's requirements.
# A direct write bypasses both.
#
# WHY AEON SESSIONS ONLY
# ----------------------
# Aeons are the senders most likely to reach for the file-system primitive instead
# of mail.sh. Binding this to non-aeon sessions blocks the operator's own
# maintenance without protecting against the actual risk
# (law-guard-binds-the-caller).
#
# WHAT IS REFUSED
# ---------------
# - Write/Edit tool calls whose file_path is under SPIRA_MAIL
# - Bash output redirections (> or >>) whose target is under SPIRA_MAIL
# - Bash mv/cp whose destination, and touch/tee/install whose argument, is under
#   SPIRA_MAIL — unless the segment invokes mail.sh
#
# WHAT IS ALLOWED
# ---------------
# - Any Bash segment that invokes mail.sh (the sanctioned writer)
# - Read operations (cat, grep, head, ls, wc, stat, …)
# - Any tool call outside an aeon session
#
# OVERRIDE: SPIRA_MAIL_WRITE_CONSIDERED=1
#
# WIRING: call as a Claude Code PreToolUse hook (empty matcher catches all tools):
#   { "matcher": "", "hooks": [{ "type": "command",
#     "command": "bash /path/to/spira/mail-fence.sh" }] }
# Source harness.sh first when the path must be derived from SPIRA_HOME:
#   "command": "bash -c '. .claude/harness.sh 2>/dev/null; bash \"$SPIRA_HOME/mail-fence.sh\"'"
set -uo pipefail

PAYLOAD="$(cat)"

# AEON SESSIONS ONLY (law-guard-binds-the-caller).
[ -n "${SPIRA_AEON:-}" ] || exit 0

# OVERRIDE — named here so the model sees it in the refusal and can cite it deliberately.
[ "${SPIRA_MAIL_WRITE_CONSIDERED:-0}" = "1" ] && exit 0
# Inline override in Bash commands.
printf '%s' "$PAYLOAD" | python3 -c '
import json, sys
try: d = json.load(sys.stdin)
except Exception: sys.exit(1)
if d.get("tool_name") != "Bash": sys.exit(1)
c = (d.get("tool_input") or {}).get("command", "")
sys.exit(0 if "SPIRA_MAIL_WRITE_CONSIDERED=1" in c else 1)
' 2>/dev/null && exit 0

# DETECTION. Pass SPIRA_MAIL via the environment so the Python block can check
# expanded paths. The payload is passed the same way (law-payload-via-env pattern
# from testenv-batch-fence.sh: a heredoc body and stdin cannot both feed python3).
hit="$(GUARD_PAYLOAD="$PAYLOAD" GUARD_SPIRA_MAIL="${SPIRA_MAIL:-}" python3 << 'PYEOF'
import json, re, sys, os

payload = os.environ.get("GUARD_PAYLOAD", "")
try:
    d = json.loads(payload)
except Exception:
    sys.exit(0)

tool = d.get("tool_name", "")
inp  = d.get("tool_input") or {}
spira_mail = os.environ.get("GUARD_SPIRA_MAIL", "")

def is_mail_token(t):
    """True if this single path token is under SPIRA_MAIL."""
    if not t:
        return False
    if "$SPIRA_MAIL" in t or "${SPIRA_MAIL}" in t:
        return True
    if spira_mail and (t == spira_mail or t.startswith(spira_mail + "/")):
        return True
    # Maildir structure fallback when SPIRA_MAIL is unset.
    return bool(re.search(r"/mail/[^/]+/(?:tmp|new|cur)(?:/|$)", t))

def seg_has_mail(seg):
    """True if the segment contains any reference to a path under SPIRA_MAIL."""
    if "$SPIRA_MAIL" in seg or "${SPIRA_MAIL}" in seg:
        return True
    if spira_mail and spira_mail in seg:
        return True
    return bool(re.search(r"/mail/[^/]+/(?:tmp|new|cur)(?:/|$)", seg))

# Write and Edit: block if file_path is under SPIRA_MAIL.
if tool in ("Write", "Edit"):
    if is_mail_token(inp.get("file_path", "")):
        print("direct-" + tool.lower())
    sys.exit(0)

if tool != "Bash":
    sys.exit(0)

c = inp.get("command", "")
if not c:
    sys.exit(0)

# Drop heredoc bodies (data, not invocations).
def drop_heredocs(s):
    for m in list(re.finditer(r"<<-?\s*[\x27\"]?(\w+)[\x27\"]?", s)):
        tag = m.group(1)
        end = re.search(r"^\s*" + re.escape(tag) + r"\s*$", s[m.end():], re.M)
        if end:
            s = s[:m.end()] + s[m.end() + end.end():]
    return s

c = drop_heredocs(c)
# Drop single-quoted spans (prose, not code paths).
c = re.sub(r"\x27[^\x27]*\x27", " ", c)

def segment_writes_to_mail(seg):
    """True if this segment writes (not reads) to a path under SPIRA_MAIL."""
    seg = seg.strip()
    if not seg:
        return False

    # Quick reject: no SPIRA_MAIL reference in this segment at all.
    if not seg_has_mail(seg):
        return False

    tokens = seg.split()

    # Output redirection: bare > or >> token followed by a mail path.
    for i, t in enumerate(tokens):
        if t in (">", ">>"):
            if i + 1 < len(tokens) and is_mail_token(tokens[i + 1]):
                return True
        # Inline redirect like "echo foo>$SPIRA_MAIL/..." (no spaces around >).
        if re.search(r">>?\s*\$\{?SPIRA_MAIL\}?", t):
            return True
        if spira_mail and re.search(r">>?\s*" + re.escape(spira_mail), t):
            return True

    # Write verbs: mv/cp destination is the last non-flag arg;
    # touch/tee/install any non-flag arg is a write target.
    for vi, t in enumerate(tokens):
        if t in ("mv", "cp"):
            args = [tok for tok in tokens[vi + 1:] if not tok.startswith("-")]
            if args and is_mail_token(args[-1]):
                return True
        elif t in ("touch", "tee", "install"):
            for tok in tokens[vi + 1:]:
                if not tok.startswith("-") and is_mail_token(tok):
                    return True

    return False

# Split into segments; a segment that invokes mail.sh is the sanctioned writer.
for seg in re.split(r"\|\||&&|[;|\n]", c):
    if re.search(r"\bmail\.sh\b", seg):
        continue
    if segment_writes_to_mail(seg):
        print("bash-write")
        sys.exit(0)
PYEOF
)"

[ -n "$hit" ] || exit 0

printf '\nBLOCKED by mail-fence — mail.sh is the only writer for mailboxes.\n\n' >&2
printf 'mail.sh send enforces atomic delivery (tmp/ then rename to new/) and lint\n' >&2
printf 'rules that every message must satisfy. A direct write bypasses both.\n\n' >&2
printf 'Send through the sanctioned writer:\n\n' >&2
printf '  bash spira/mail.sh send <mailbox> --from "<name>" --subject "<s>" [--kind K] < body\n\n' >&2
printf 'Override when a test fixture must seed a mailbox directly:\n' >&2
printf '  SPIRA_MAIL_WRITE_CONSIDERED=1\n' >&2
exit 2
