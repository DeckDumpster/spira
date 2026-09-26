#!/usr/bin/env bash
#
# tmux-scope-fence.sh — refuse a suite that can reach the default tmux socket.
#
#   tmux-scope-fence.sh
#
# A suite that calls a tmux session-affecting command with no socket scoping drives
# whatever server the caller's environment already points at — the operator's own,
# on the host where this incident happened twice (sp-pfca0). A suite is scoped when
# either the offending line carries -L/-S, or the file sets TMUX_TMPDIR anywhere, or
# (for concierge.sh, whose own socket defaults to "concierge") the file sets
# CONCIERGE_SOCKET or CONCIERGE_SESSION anywhere.
#
# Scans spira/test-*.sh for non-comment lines invoking a tmux session command
# (new-session, kill-server, kill-session, has-session, attach[-session],
# send-keys, list-sessions, list-panes, list-clients, set-option, respawn-pane,
# start-server, new-window, show-environment, split-window, select-pane,
# select-window, kill-window, rename-session, display-message, source-file)
# without -L/-S and with no file-wide TMUX_TMPDIR, and for lines invoking
# concierge.sh with start/wake/here/stop with no file-wide CONCIERGE_SOCKET or
# CONCIERGE_SESSION.
#
# Exits 0 when none found, 1 when offenders are present, 3 when the tree cannot
# be examined (law-absence-needs-a-positive-control).
# covers: spira/test-*.sh concierge.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
ROOT="$(git -C "$HERE" rev-parse --show-toplevel 2>/dev/null)"
[ -n "$ROOT" ] || ROOT="$(cd "$HERE/.." && pwd -P)"

TMUX_CMDS='new-session|kill-server|kill-session|has-session|attach-session|attach|send-keys|list-sessions|list-panes|list-clients|set-option|respawn-pane|start-server|new-window|show-environment|split-window|select-pane|select-window|kill-window|rename-session|display-message|source-file'

shopt -s nullglob
suites=("$ROOT"/spira/test-*.sh)
if [ "${#suites[@]}" -eq 0 ]; then
    printf 'tmux-scope-fence: refusing to report clean — no spira/test-*.sh files found\n' >&2
    exit 3
fi

offenders=""
for f in "${suites[@]}"; do
    rel="${f#"$ROOT"/}"
    case "$rel" in */tmux-scope-fence.sh|tmux-scope-fence.sh) continue ;; esac

    has_tmux_tmpdir=0
    grep -q 'TMUX_TMPDIR' "$f" 2>/dev/null && has_tmux_tmpdir=1
    has_concierge_sock=0
    grep -qE 'CONCIERGE_SOCKET|CONCIERGE_SESSION' "$f" 2>/dev/null && has_concierge_sock=1

    while IFS= read -r entry; do
        lineno="${entry%%:*}"
        text="${entry#*:}"
        printf '%s' "$text" | grep -qE -- '-L[[:space:]]' && continue
        if [ "$has_tmux_tmpdir" -eq 1 ]; then continue; fi
        offenders="${offenders}${rel}:${lineno}: ${text}
"
    done < <(grep -nE "(^|[;&|\`]|\\\$\\() *tmux ($TMUX_CMDS)\\b" "$f" 2>/dev/null | grep -v ':[[:space:]]*#')

    if [ "$has_concierge_sock" -eq 0 ]; then
        while IFS= read -r entry; do
            lineno="${entry%%:*}"
            text="${entry#*:}"
            offenders="${offenders}${rel}:${lineno}: ${text}
"
        done < <(grep -nE 'concierge\.sh["'"'"']?[[:space:]]+(start|wake|here|stop)\b' "$f" 2>/dev/null | grep -v ':[[:space:]]*#')
    fi
done

if [ -z "$offenders" ]; then
    printf 'tmux-scope-fence: no unscoped tmux/concierge.sh invocation in %d suite(s)\n' "${#suites[@]}"
    exit 0
fi

printf 'tmux-scope-fence: unscoped tmux/concierge.sh invocation reaches the default socket:\n' >&2
printf '%s' "$offenders" | sed 's/^/tmux-scope-fence:   /' >&2
printf 'tmux-scope-fence: scope every tmux call with -L/-S, or export TMUX_TMPDIR; scope\n' >&2
printf 'tmux-scope-fence: every concierge.sh start/wake/here/stop with CONCIERGE_SOCKET\n' >&2
exit 1
