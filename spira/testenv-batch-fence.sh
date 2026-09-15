#!/usr/bin/env bash
#
# testenv-batch-fence.sh — refuse direct spira/test-*.sh runs in aeon sessions.
#
#   bash testenv-batch-fence.sh    reads Claude Code's PreToolUse JSON on stdin
#
# Exit 2 BLOCKS the tool call and feeds stderr back to the model. Exit 0 allows.
#
# WHY THIS EXISTS
# ---------------
# Builder aeons iterate by running suites directly on the host: `bash spira/test-x.sh`.
# That violates law-tests-run-only-through-testenv-batch for two reasons:
# (a) the suite can reach ambient host config and the production store through env vars
#     that testenv-batch.sh strips from the container; and (b) it competes for the same
# four cores as the landing gate's own container batch, which produced EAGAIN reds.
#
# Measured from aeon session logs: 21 of the last 23 bead sessions contain direct host
# runs. The rule appeared only as a slug in the statute index — never as full text —
# because builder's FAYTH_STATUTE_CORE did not include it. Re-violation at that scale
# is the ladder's trigger for a mechanism (rung four).
#
# WHY AEON SESSIONS ONLY
# ----------------------
# The offenders all had SPIRA_AEON set. A fence on the brain session would bind the
# most disciplined caller and leave the actor untouched (law-guard-binds-the-caller).
# An interactive session running a suite by hand to understand it is exactly the kind
# of work a fence must not interrupt.
#
# WHAT IS REFUSED
# ---------------
# A Bash tool call whose command — after stripping heredoc bodies and single-quoted spans
# — contains a segment that executes spira/test-*.sh via bash or sh (without -n) or via
# the `time` prefix, unless that segment routes through testenv-batch.sh.
#
# WHAT IS ALLOWED
# ---------------
# - bash spira/testenv-batch.sh ... (the correct path)
# - grep/head/cat/bash -n on a test file (reading, not running)
# - any invocation outside an aeon session (SPIRA_AEON unset)
#
# OVERRIDE: TESTENV_BATCH_FENCE_OVERRIDE=1
#
# WIRING: call as a Claude Code PreToolUse hook for Bash events. Register via the
# operator's settings file alongside the other aeon guards:
#   { "matcher": "Bash", "hooks": [{ "type": "command",
#     "command": "bash /path/to/spira/testenv-batch-fence.sh" }] }
# Or source harness.sh for a path derived from SPIRA_HOME:
#   "command": "bash -c '. .claude/harness.sh 2>/dev/null; bash \"$SPIRA_HOME/testenv-batch-fence.sh\"'"
set -uo pipefail

# Drain stdin unconditionally so the caller's pipe is not left open (which would
# cause a BrokenPipeError in the process writing the PreToolUse payload).
PAYLOAD="$(cat)"

# AEON SESSIONS ONLY. An interactive session running a suite directly to understand it
# is legitimate. Binding this to non-aeon sessions would block the wrong actor
# (law-guard-binds-the-caller).
[ -n "${SPIRA_AEON:-}" ] || exit 0

# OVERRIDE. Named here so the model sees it in the refusal and can cite it deliberately.
[ "${TESTENV_BATCH_FENCE_OVERRIDE:-0}" = "1" ] && exit 0
printf '%s' "$PAYLOAD" | python3 -c '
import json, sys
try: d = json.load(sys.stdin)
except Exception: sys.exit(1)
c = (d.get("tool_input") or {}).get("command", "")
sys.exit(0 if "TESTENV_BATCH_FENCE_OVERRIDE=1" in c else 1)
' 2>/dev/null && exit 0

# DETECTION. Extract the Bash command, strip data from code, and look for a segment
# that runs spira/test-*.sh outside of testenv-batch.sh.
#
# Why strip heredocs and single-quoted spans: a guard that fires when you WRITE A
# COMMENT about the rule is a guard that punishes documentation. Drop data; match only
# the code structure that survives.
#
# THE PAYLOAD IS PASSED VIA ENVIRONMENT, not stdin. A heredoc body provides stdin to
# the python3 process; piping the payload at the same time as using a heredoc for the
# source code would have stdin contested — the heredoc wins and the payload is lost.
hit="$(GUARD_PAYLOAD="$PAYLOAD" python3 << 'PYEOF'
import json, re, sys, os

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

# --- strip heredoc bodies ---------------------------------------------------
# <<EOF / <<-"EOF" ... terminator on its own line. Heredoc bodies are data.
def drop_heredocs(s):
    for m in list(re.finditer(r"<<-?\s*[\x27\"]?(\w+)[\x27\"]?", s)):
        tag = m.group(1)
        end = re.search(r"^\s*" + re.escape(tag) + r"\s*$", s[m.end():], re.M)
        if end:
            s = s[:m.end()] + s[m.end() + end.end():]
    return s

c = drop_heredocs(c)

# --- strip single-quoted spans (prose, not invocations) ---------------------
c = re.sub(r"\x27[^\x27]*\x27", " ", c)

# --- split into command segments --------------------------------------------
# Split on pipeline/sequence operators so each atomic command is examined
# independently. `cd /x && bash spira/test-x.sh` becomes two segments; the
# bash segment is then evaluated on its own.
segments = re.split(r"\|\||&&|[;|\n]", c)

# BATCH_PAT: the one command that is allowed to run a test suite directly.
BATCH_PAT = re.compile(r"testenv-batch\.sh")

# SHELLS: verbs that execute a script file (not read it).
SHELLS = frozenset({"bash", "sh", "/bin/bash", "/bin/sh", "/usr/bin/bash", "/usr/bin/sh"})

def segment_executes_test(seg):
    """Return the matched test filename if this segment directly executes a test suite,
    None if it only reads it or routes through testenv-batch.sh."""
    seg = seg.strip()
    if not seg:
        return None

    # Whole segment mentions testenv-batch.sh -> correct path, allow.
    if BATCH_PAT.search(seg):
        return None

    # Does this segment contain any spira/test-*.sh path at all?
    if not re.search(r"(?:spira/)?test-[^/\s]+\.sh\b", seg):
        return None

    # Tokenise the segment to find the command verb.
    parts = seg.split()
    i = 0
    # Skip leading env-var assignments (KEY=VALUE without a leading dash).
    while i < len(parts) and re.match(r"^[A-Za-z_][A-Za-z0-9_]*=", parts[i]):
        i += 1
    if i >= len(parts):
        return None

    verb = parts[i]

    # `time` is a timing prefix, not the shell: skip it and advance to the real verb.
    if verb == "time":
        i += 1
        while i < len(parts) and re.match(r"^[A-Za-z_][A-Za-z0-9_]*=", parts[i]):
            i += 1
        if i >= len(parts):
            return None
        verb = parts[i]

    # Only bash/sh can execute a script via a path argument.
    if verb not in SHELLS:
        # Any other verb (grep, head, cat, tail, …) is reading, not running.
        return None

    # bash is the verb — now collect its flags.
    i += 1
    has_n = False
    while i < len(parts) and parts[i].startswith("-") and parts[i] != "--":
        flag = parts[i]
        if not flag.startswith("--"):
            if "n" in flag:
                has_n = True  # bash -n: syntax check only, not execution
            if "c" in flag:
                # bash -c: the script is a string argument, not a file path.
                return None
        i += 1

    if has_n:
        return None  # bash -n <file> is a read, not a run

    # The remaining positional arguments should include the script path.
    for j in range(i, len(parts)):
        p = parts[j]
        if p.startswith("-"):
            continue
        if re.search(r"(?:^|/)test-[^/]+\.sh$", p) and "testenv-batch" not in p:
            return p
        # Stop at an embedded operator token.
        if any(op in p for op in ("&&", "||", ";")):
            break

    return None

for seg in segments:
    result = segment_executes_test(seg)
    if result:
        print(result)
        sys.exit(0)
PYEOF
)"

[ -n "$hit" ] || exit 0

printf '\nBLOCKED by testenv-batch-fence (law-tests-run-only-through-testenv-batch).\n\n' >&2
printf 'Direct suite runs on the host reach ambient config, the production store,\n' >&2
printf 'and the same cores the landing gate uses — which produced fork-EAGAIN reds.\n\n' >&2
printf 'Run it through testenv-batch.sh instead:\n\n' >&2
printf '  bash spira/testenv-batch.sh --suites %s <branch>\n\n' "$hit" >&2
printf 'To run all coverage-selected suites for a branch:\n\n' >&2
printf '  bash spira/testenv-batch.sh <branch>\n\n' >&2
printf 'Reading a suite (grep, head, cat, bash -n) is never refused.\n\n' >&2
printf 'Override when the run is intentional and isolated:\n' >&2
printf '  TESTENV_BATCH_FENCE_OVERRIDE=1\n' >&2
exit 2
