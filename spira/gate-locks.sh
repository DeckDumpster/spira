#!/usr/bin/env bash
# gate-locks.sh — report HELD/FREE status of every gate lock file.
#
#   gate-locks.sh [<run-dir>]
#
# The PID in /proc/locks is the flock acquirer and is routinely dead while the lock is
# still held by processes that inherited the fd (gate.sh closes fd 9 before the gate
# command, but not before earlier preflight forks). Resolves the real holder via the
# .lock.holder file gate.sh writes at acquisition: PID PGID. Falls back to /proc/locks
# only for locks that predate holder recording.
#
# STALE means the holder file names a dead PID and dead PGID, but flock -n cannot acquire
# the lock — it is held by a process outside the recorded group and needs manual
# investigation. A lock that flock -n can acquire is FREE regardless of any stale holder
# file.
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/lib.sh"

RUN="${1:-$SPIRA_RUN}"
LOCK_DIR="${RUN}/worktree"

[ -d "$LOCK_DIR" ] || { printf 'gate-locks: %s: not a directory\n' "$LOCK_DIR" >&2; exit 1; }

# _pid_alive <pid> — 0 if the process exists
_pid_alive() { [ -d "/proc/$1" ] 2>/dev/null; }

# _pgid_live_member <pgid> — prints first live PID in the group, or nothing
_pgid_live_member() {
    pgrep -g "$1" 2>/dev/null | head -1 || true
}

# _pid_age <pid> — human-readable elapsed time since process started
_pid_age() {
    local start hz boot age_s
    start="$(awk '{print $22}' "/proc/$1/stat" 2>/dev/null)" || { printf '?'; return; }
    hz="$(getconf CLK_TCK 2>/dev/null)" || hz=100
    boot="$(awk '/^btime/{print $2}' /proc/stat 2>/dev/null)" || { printf '?'; return; }
    age_s=$(( $(date +%s) - (boot + start / hz) ))
    if   [ "$age_s" -ge 3600 ]; then printf '%dh%dm' $(( age_s/3600 )) $(( (age_s%3600)/60 ))
    elif [ "$age_s" -ge 60 ];   then printf '%dm%ds'  $(( age_s/60 )) $(( age_s%60 ))
    else                              printf '%ds'     "$age_s"
    fi
}

# _in_proc_locks_inode <file> — prints PID if an FLOCK WRITE lock exists for this inode
_in_proc_locks_inode() {
    local ino_hex
    ino_hex="$(printf '%x' "$(stat -c '%i' "$1" 2>/dev/null)")" || return 1
    awk -v sfx=":$ino_hex" '$2=="FLOCK" && $5 ~ (sfx "[ \t]*$") {print $4; exit}' \
        /proc/locks 2>/dev/null || true
}

printf '%-48s  %-6s  %-8s  %-7s  %s\n' lock status holder age note
printf '%-48s  %-6s  %-8s  %-7s  %s\n' ---- ------ ------ --- ----

found=0
stale_count=0

for lockfile in "$LOCK_DIR"/.gate.*.lock; do
    [ -f "$lockfile" ] || continue
    found=$(( found + 1 ))

    label="${lockfile##*/}"; label="${label%.lock}"; label="${label#.gate.}"
    holder_file="${lockfile}.holder"

    status=FREE; holder_pid="—"; age="—"; note=""

    # Fast HELD/FREE determination via non-blocking flock attempt.
    if flock -n "$lockfile" /bin/true 2>/dev/null; then
        status=FREE
    else
        status=HELD
        if [ -f "$holder_file" ]; then
            rec_pid="" rec_pgid=""
            read -r rec_pid rec_pgid _ < "$holder_file" 2>/dev/null || true

            if [ -n "$rec_pid" ] && _pid_alive "$rec_pid"; then
                holder_pid="$rec_pid"
                age="$(_pid_age "$rec_pid")"
            elif live="$(_pgid_live_member "${rec_pgid:-0}")" && [ -n "$live" ]; then
                # Original holder exited; process group still has the fd.
                holder_pid="$live"
                age="$(_pid_age "$live")"
                note="inherited fd"
            else
                # Holder PID and PGID both dead, but flock says locked.
                status=STALE
                stale_count=$(( stale_count + 1 ))
                note="dead holder — remove ${lockfile##*/} to clear"
            fi
        else
            # No holder file: gate predates holder recording. Fall back to /proc/locks.
            locks_pid="$(_in_proc_locks_inode "$lockfile")"
            if [ -n "$locks_pid" ]; then
                if _pid_alive "$locks_pid"; then
                    holder_pid="$locks_pid"
                    age="$(_pid_age "$locks_pid")"
                else
                    # /proc/locks PID dead but lock is held — inherited fd, can't identify.
                    holder_pid="$locks_pid"
                    note="pid dead; gate predates holder recording"
                fi
            fi
        fi
    fi

    printf '%-48s  %-6s  %-8s  %-7s  %s\n' "$label" "$status" "$holder_pid" "$age" "$note"
done

if [ "$found" -eq 0 ]; then
    printf '  (no gate lock files in %s)\n' "$LOCK_DIR"
fi

if [ "$stale_count" -gt 0 ]; then
    printf '\n%d stale lock(s). A stale flock with no live holder is likely a kernel artifact;\n' \
        "$stale_count"
    printf 'verify with: flock -n <lockfile> echo free  — then remove the file if confirmed.\n'
fi
