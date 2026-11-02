#!/usr/bin/env bash
# runbook.sh <entry> [NAME=value ...] — run the `<!-- check: <entry> -->` block of
# docs/runbooks/rounds.md in this world, each line through the shell, and require the exit the
# line names. A line that gives its exit is not run again; a `retried` line that has not yet
# exits 3 to be called again. Exit 0 once every line gave its exit, 9 on a line that exits
# otherwise or names no exit.
set -u
entry="$1"
shift
for kv in "$@"; do export "${kv?}"; done
state="$SIM_WORLD/runbook.$entry.done"
done_n=$(cat "$state" 2>/dev/null || echo 0)
block=$(git show HEAD:docs/runbooks/rounds.md | awk -v head="<!-- check: $entry -->" '
    $0 == head { found = 1; next }
    found && !open && /^```sh$/ { open = 1; next }
    open && /^```$/ { exit }
    open { print }')
[ -n "$block" ] || { echo "runbook: no check block $entry" >&2; exit 9; }
n=0
while IFS= read -r line; do
    n=$((n + 1))
    [ "$n" -le "$done_n" ] && continue
    case "$line" in *'   # '*) ;; *) echo "runbook $entry:$n: names no exit: $line" >&2; exit 9 ;; esac
    cmd="${line%   # *}" note="${line##*   # }"
    case "$note" in
    'not checked: '*) echo "runbook $entry:$n: $note"; echo "$n" > "$state"; continue ;;
    'exits '[0-9]*', retried') want="${note#exits }" want="${want%%,*}" retry=1 ;;
    'exits '[0-9]*) want="${note#exits }" retry=0 ;;
    *) echo "runbook $entry:$n: unreadable exit note: $note" >&2; exit 9 ;;
    esac
    out=$(eval "$cmd" 2>&1)
    rc=$?
    echo "runbook $entry:$n: $cmd -> $rc"
    if [ "$rc" != "$want" ]; then
        printf '%s\n' "$out" | tail -n 15 | sed "s|^|runbook $entry:$n: |"
        [ "$retry" = 1 ] && exit 3
        echo "runbook $entry:$n: exited $rc, the runbook says $want" >&2
        exit 9
    fi
    echo "$n" > "$state"
done <<< "$block"
exit 0
