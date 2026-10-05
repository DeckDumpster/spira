#!/usr/bin/env bash
#
# test-script-running-guard.sh — a write to a shell script a live process is executing is refused.
#
# tier: T1
# covers: spira/script-running-guard.sh aeon/src/run.rs
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

echo "test-script-running-guard.sh"

T="$(mktemp -d)"
trap 'kill "${RPID:-}" 2>/dev/null; rm -rf "$T"' EXIT
printf '#!/usr/bin/env bash\nsleep 60\n' > "$T/running.sh"
printf '#!/usr/bin/env bash\nexit 0\n' > "$T/idle.sh"
bash "$T/running.sh" & RPID=$!
sleep 0.3

guard() {   # guard <tool> <file_path|command> [env...] -> BLOCK or ALLOW
    local tool="$1" arg="$2"; shift 2
    local key=file_path; [ "$tool" = Bash ] && key=command
    if printf '%s' "$arg" | python3 -c 'import json,sys; print(json.dumps({"tool_name":sys.argv[1],"cwd":sys.argv[3],"tool_input":{sys.argv[2]:sys.stdin.read()}}))' "$tool" "$key" "$T" \
        | env -i PATH="$PATH" "$@" bash "$HERE/script-running-guard.sh" >/dev/null 2>&1; then echo ALLOW; else echo BLOCK; fi
}

is "Edit to a running script is refused" BLOCK "$(guard Edit "$T/running.sh")"
is "Write to a running script is refused" BLOCK "$(guard Write "$T/running.sh")"
is "a relative path resolves against cwd" BLOCK "$(guard Write "./running.sh")"
is "a redirect into a running script is refused" BLOCK "$(guard Bash "echo x >> $T/running.sh")"
is "python open(w) of a running script is refused" BLOCK \
    "$(guard Bash "python3 -c \"open('$T/running.sh','w').write('x')\"")"
is "sed -i of a running script is refused" BLOCK "$(guard Bash "sed -i s/a/b/ $T/running.sh")"
is "Write to a script nothing runs is allowed" ALLOW "$(guard Write "$T/idle.sh")"
is "redirect into an idle script is allowed" ALLOW "$(guard Bash "echo x > $T/idle.sh")"
is "write to a non-script path is allowed" ALLOW "$(guard Write "$T/notes.txt")"
is "mv-replace over a running script is allowed" ALLOW \
    "$(guard Bash "printf x > $T/running.sh.new && mv $T/running.sh.new $T/running.sh")"
is "writing a sibling temp file is allowed" ALLOW "$(guard Write "$T/running.sh.new")"
is "the override lets the write through" ALLOW "$(guard Edit "$T/running.sh" SCRIPT_RUNNING_EDIT_CONSIDERED=1)"
kill "$RPID"; wait "$RPID" 2>/dev/null
is "once the process exits the write is allowed" ALLOW "$(guard Edit "$T/running.sh")"

tl_summary
