#!/usr/bin/env bash
# aeon-fence.sh — PreToolUse hook: refuse queue-operating commands in aeon sessions.
#
# Registered by aeon.sh via --settings so it is bound to the aeon, not to a global
# profile (law-guard-binds-the-caller). Only fires when SPIRA_AEON is set.
#
# OVERRIDE (Ops incidents only): the operator sets SPIRA_AEON_OVERRIDE=1 before
# starting the session; an aeon cannot set it from inside a running session.
#
# EXIT: 0 always. Block by printing {"decision":"block","reason":"..."} to stdout.
set -uo pipefail
[ -n "${SPIRA_AEON:-}" ] || exit 0

[ -z "${SPIRA_AEON_OVERRIDE:-}" ] || {
    printf 'aeon-fence: SPIRA_AEON_OVERRIDE set — bypassing fence (aeon=%s)\n' "$SPIRA_AEON" >&2
    exit 0
}

payload="$(cat 2>/dev/null || true)"

tool="$(printf '%s' "$payload" | python3 -c '
import json, sys
try: d = json.load(sys.stdin); print(d.get("tool_name",""))
except Exception: print("")' 2>/dev/null)"

[ "$tool" = "Bash" ] || exit 0

cmd="$(printf '%s' "$payload" | python3 -c '
import json, sys
try: d = json.load(sys.stdin); print(d.get("tool_input",{}).get("command",""))
except Exception: print("")' 2>/dev/null)"

[ -n "$cmd" ] || exit 0

# Returns "1" if /$1 appears as an exec target in $2; "" if only as a git file argument.
# Splits compound commands on shell separators; sub-commands starting with "git" are
# file operations, not invocations.
_exec_ctx() {
    python3 -c '
import sys, re
script = sys.argv[1]; cmd = sys.argv[2]
for sub in re.split(r"&&|\|\||[;\n]", cmd):
    sub = sub.strip()
    if ("/" + script) not in sub: continue
    toks = sub.split()
    if not toks or toks[0] == "git": continue
    print("1"); break
' "$1" "$2" 2>/dev/null
}

reason=""

for _script in landing.sh batch.sh verdict.sh slay.sh world.sh deploy.sh activate.sh promote.sh; do
    case "$cmd" in
        *"/$_script"*)
            [ "$(_exec_ctx "$_script" "$cmd")" = "1" ] && { reason="aeons may not call $_script (sp-kz8ob: landing and batch handle forge writes; use SPIRA_AEON_OVERRIDE=1 for Ops incidents)"; break; } ;;
    esac
done

if [ -z "$reason" ]; then
    case "$cmd" in
        *"/queue.sh"*)
            case "$cmd" in
                *"/queue.sh stats"*) ;;
                *)
                    [ "$(_exec_ctx "queue.sh" "$cmd")" = "1" ] && \
                        reason="aeons may not operate the queue (sp-kz8ob: queue.sh stats is the only read-only subcommand; use SPIRA_AEON_OVERRIDE=1 for Ops incidents)" ;;
            esac ;;
    esac
fi

if [ -z "$reason" ]; then
    case "$cmd" in
        *"gh pr create"*|*"gh pr edit"*|*"gh pr merge"*|*"gh pr close"*|\
        *"gh release create"*|*"gh release edit"*|*"gh release delete"*|\
        *"gh workflow run"*|\
        *"gh run rerun"*|*"gh run cancel"*)
            reason="aeons carry no forge credentials (sp-kz8ob: forge writes go through landing.sh and batch.sh)" ;;
    esac
fi

if [ -z "$reason" ]; then
    case "$cmd" in
        *"git push"*|*"git -C"*" push "*)
            reason="aeons carry no push credentials (sp-kz8ob: landing.sh and batch.sh handle all merges and pushes)" ;;
    esac
fi

if [ -z "$reason" ] && [ -n "${SPIRA_RUN:-}" ]; then
    case "$cmd" in
        *"${SPIRA_RUN}/landstate"*|*"${SPIRA_RUN}/queue"*)
            reason="aeons may not write to \$SPIRA_RUN/landstate or \$SPIRA_RUN/queue (sp-kz8ob)" ;;
    esac
fi

if [ -z "$reason" ] && [ -n "${SPIRA_PROD:-}" ]; then
    # Refuse write shapes only; reads and script execution from prod are permitted.
    case "$cmd" in
        *">${SPIRA_PROD}"*|*"> ${SPIRA_PROD}"*|\
        *">>${SPIRA_PROD}"*|*">> ${SPIRA_PROD}"*|\
        *"rm "*"${SPIRA_PROD}"*|\
        *"mv "*"${SPIRA_PROD}"*|\
        *"tee"*"${SPIRA_PROD}"*|\
        *"sed -i"*"${SPIRA_PROD}"*|\
        *"git"*"-C"*"${SPIRA_PROD}"*" commit"*|\
        *"git"*"-C"*"${SPIRA_PROD}"*" reset"*|\
        *"git"*"-C"*"${SPIRA_PROD}"*" checkout"*|\
        *"git"*"-C"*"${SPIRA_PROD}"*" clean"*|\
        *"git"*"-C"*"${SPIRA_PROD}"*" add"*)
            reason="aeons may not write to the production checkout \$SPIRA_PROD (sp-kz8ob: use SPIRA_AEON_OVERRIDE=1 for Ops incidents)" ;;
    esac
fi

[ -n "$reason" ] || exit 0

printf 'aeon-fence: BLOCKED aeon=%s bead=%s: %s\n' \
    "${SPIRA_AEON:-?}" "${BEAD_ID:-?}" "$reason" >&2

printf '{"decision":"block","reason":"%s"}\n' \
    "$(printf '%s' "$reason" | sed 's/"/\\"/g')"
exit 0
