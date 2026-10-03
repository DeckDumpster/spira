#!/usr/bin/env bash
# dolt-tmp-prune.sh — bounds the live dolt-beads server's own temp spool
# (SPIRA_DOLT_DATA/.dolt/tmp): spill files Dolt writes and never removes when the
# server process dies. Nothing swept these before: 1,211 files and 10GB accumulated
# there in four days of restart-limit crashes (sp-n1l7y), filling the root filesystem
# twice.
#
# Sourced by its own test for the two pure functions below; run directly (as the
# dolt-tmp-prune.service ExecStart) to prune the live server's spool.
# covers: systemd/dolt-tmp-prune.service systemd/dolt-tmp-prune.timer

# _dolt_tmp_held_fds <pid> — every absolute path a live process still has open, one
# per line, resolved through /proc/<pid>/fd via readlink (not `ls -l`'s free-form
# text column, which breaks on a filename containing a space). Empty output, never a
# failure, when <pid> is empty, "0", or the process is already gone: a dead server
# holds nothing open and every one of its spill files is prunable.
_dolt_tmp_held_fds() {
    local pid="$1" fd link
    case "$pid" in ''|0) return 0 ;; esac
    [ -d "/proc/$pid/fd" ] || return 0
    for fd in "/proc/$pid/fd"/*; do
        [ -e "$fd" ] || continue
        link="$(readlink -f "$fd" 2>/dev/null)" || continue
        [ -n "$link" ] && printf '%s\n' "$link"
    done
}

# _dolt_tmp_prune_stale <tmp-dir> <window-seconds> <held-newline-list> — removes
# every immediate entry of <tmp-dir> whose mtime is older than <window-seconds>,
# unless <held-newline-list> (paths from _dolt_tmp_held_fds) names it or a path
# under it — never delete a spool the live server holds open, the safety rule the
# 2026-09-27 incident used by hand. One "pruned <path> (<KiB>KiB freed, age <age>s
# >= window <window>s)" line per removal on stdout.
_dolt_tmp_prune_stale() {
    local dir="$1" window="$2" held="$3" now entry age sz_kb h held_it
    [ -d "$dir" ] || return 0
    now="$(date +%s)"
    while IFS= read -r -d '' entry; do
        entry="${entry%/}"
        held_it=0
        while IFS= read -r h; do
            case "$h" in
                "$entry"|"$entry"/*) held_it=1; break ;;
            esac
        done <<<"$held"
        [ "$held_it" = 1 ] && continue
        age=$(( now - $(stat -c %Y "$entry" 2>/dev/null || printf '%s' "$now") ))
        [ "$age" -lt "$window" ] && continue
        sz_kb="$(du -sk "$entry" 2>/dev/null | cut -f1)"
        rm -rf "$entry" 2>/dev/null || continue
        printf 'pruned dolt tmp entry %s (%sKiB freed, age %ss >= window %ss)\n' \
            "$entry" "${sz_kb:-0}" "$age" "$window"
    done < <(find "$dir" -mindepth 1 -maxdepth 1 -print0 2>/dev/null)
}

main() {
    set -uo pipefail
    . "$(dirname "$0")/lib.sh"
    if [ -z "${SPIRA_DOLT_DATA:-}" ]; then
        echo "dolt-tmp-prune: SPIRA_DOLT_DATA is empty — this install does not manage the server, nothing to prune" >&2
        return 0
    fi
    local pid held
    pid="$(systemctl --user show -p MainPID --value dolt-beads.service 2>/dev/null)"
    held="$(_dolt_tmp_held_fds "${pid:-}")"
    _dolt_tmp_prune_stale "$SPIRA_DOLT_DATA/.dolt/tmp" "${SPIRA_DOLT_TMP_TTL:-86400}" "$held"
}

if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
    main "$@"
fi
