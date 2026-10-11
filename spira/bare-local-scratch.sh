#!/usr/bin/env bash
# bare-local-scratch.sh — the scratch dir bare-local.sh stages a release in, and the reaper for
# the ones a killed run left behind. Sourced, never run. Releases are read-only trees, so a plain
# rm -rf leaves them; a dir on tmpfs that survives holds RAM.
# covers: spira/bare-local.sh spira/bare-local-scratch.sh

bare_local_rm() {
    chmod -R u+w "$1" 2>/dev/null
    rm -rf "$1"
}

bare_local_cleanup() {
    [ -n "${scratch:-}" ] && bare_local_rm "$scratch"
}

# bare_local_scratch_new <root> — sets $scratch; the dir is removed on every exit path
# and carries a lock the reaper reads as "a run still holds this".
bare_local_scratch_new() {
    scratch="$(mktemp -d "$1/bare-local.XXXXXX")" || return 1
    exec 9>"$scratch/.lock"
    flock -n 9 || return 1
    trap bare_local_cleanup EXIT
    trap 'exit 143' TERM
    trap 'exit 130' INT
    trap 'exit 129' HUP
}

# bare_local_reap <root> <cap-seconds> — removes bare-local dirs older than the cap that no
# process holds the lock of.
bare_local_reap() {
    local root="$1" cap="$2" d now age
    now="$(date +%s)"
    for d in "$root"/bare-local.??????; do
        [ -d "$d" ] || continue
        age=$(( now - $(stat -c %Y "$d" 2>/dev/null || printf '%s' "$now") ))
        [ "$age" -ge "$cap" ] || continue
        (
            if [ -e "$d/.lock" ]; then
                exec 8<"$d/.lock" && flock -n 8 || exit 1
            fi
            bare_local_rm "$d"
        ) || continue
        printf 'bare-local: reaped %s (age %ss >= cap %ss)\n' "$d" "$age" "$cap" >&2
    done
}
