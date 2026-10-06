#!/usr/bin/env bash
#
# test-aeon-fence-exec-ctx.sh — the fence's guarded-script matcher reads commands, not prose.
#
# tier: T1
# covers: spira/hooks/aeon-fence.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

echo "test-aeon-fence-exec-ctx.sh"

fence() {   # fence <command-string> -> prints BLOCK or ALLOW
    local out
    out="$(printf '%s' "$1" | python3 -c 'import json,sys; print(json.dumps({"tool_name":"Bash","tool_input":{"command":sys.stdin.read()}}))' \
        | env -i PATH="$PATH" SPIRA_AEON=t BEAD_ID=t bash "$HERE/hooks/aeon-fence.sh" 2>/dev/null)"
    case "$out" in *'"decision":"block"'*) echo BLOCK ;; *) echo ALLOW ;; esac
}

is "positive control: direct call is blocked" BLOCK "$(fence 'landing.sh go')"
is "positive control: after a newline is blocked" BLOCK "$(fence $'echo hi\nlanding.sh go')"
is "positive control: a shell reading a heredoc is blocked" BLOCK "$(fence $'bash <<E\nlanding.sh go\nE')"
is "prose in a heredoc body to bd close is allowed" ALLOW \
    "$(fence $'bd close x --reason-file - <<\'R\'\nsee foo; deploy.sh/promote.sh bar\nlanding.sh go\nR')"
is "a command after the heredoc terminator is still checked" BLOCK \
    "$(fence $'cat <<R\nprose\nR\nlanding.sh go')"

fence_read() {   # fence_read <file> -> BLOCK or ALLOW for the Read tool
    local out
    out="$(printf '%s' "$1" | python3 -c 'import json,sys; print(json.dumps({"tool_name":"Read","tool_input":{"file_path":sys.stdin.read()}}))' \
        | env -i PATH="$PATH" SPIRA_AEON=t BEAD_ID=t bash "$HERE/hooks/aeon-fence.sh" 2>/dev/null)"
    case "$out" in *'"decision":"block"'*) echo BLOCK ;; *) echo ALLOW ;; esac
}

is "the admin credential cannot be read with the Read tool" BLOCK "$(fence_read /home/x/.config/spira/spira-lc-admin.credential)"
is "the admin credential cannot be catted" BLOCK "$(fence 'cat ~/.config/spira/spira-lc-admin.credential')"
is "the service credential is not the admin credential's fence" ALLOW "$(fence_read /home/x/.config/spira/spira-lc.credential)"

tl_summary
