# cargo-target-bins-prune.sh — bounds testenv-batch.sh's --with-bins cache
# (SPIRA_RUN/cargo-target-bins/<tree-hash>/, sp-uq6up): nothing removed an entry
# there before, so one ~200MB directory accumulated per distinct tree ever
# tested — every certification, every round, every attribution run — until the
# disk filled twice in one day.
#
# Sourced by testenv-batch.sh; also sourced directly by its test so the sweep
# can be exercised against a fixture directory without cargo or podman.
# covers: spira/testenv-batch.sh

# _bins_prune_stale <base-dir> <keep-tree> <window-seconds>
# Removes every immediate subdirectory of <base-dir> except <keep-tree> — the
# tree testenv-batch.sh is about to build, never pruned regardless of age —
# whose mtime is older than <window-seconds>. An entry a concurrent run still
# holds a SHARED flock on (testenv-batch.sh holds one on "<entry>.lock" for its
# whole build+copy) survives even past the window: the EXCLUSIVE, non-blocking
# probe here fails whenever that shared lock is held, by design — flock offers
# no other way to ask "is anyone using this?" without also being able to
# acquire it (watchd.sh: there is no such thing as a stale flock, the kernel
# drops it when the holder's last fd closes, so a lock that cannot be taken
# right now means a holder that is really still there).
# One "pruned <tree> (<KiB>KiB freed)" line per removal on stdout.
_bins_prune_stale() {
    local base="$1" keep="$2" window="$3" now entry tree age sz_kb
    [ -d "$base" ] || return 0
    now="$(date +%s)"
    while IFS= read -r -d '' entry; do
        entry="${entry%/}"
        tree="$(basename "$entry")"
        [ "$tree" = "$keep" ] && continue
        age=$(( now - $(stat -c %Y "$entry" 2>/dev/null || printf '%s' "$now") ))
        [ "$age" -lt "$window" ] && continue
        if [ -e "$entry.lock" ] && ! flock -n -x "$entry.lock" true 2>/dev/null; then
            continue  # a concurrent run holds this entry — never prune what is in use
        fi
        sz_kb="$(du -sk "$entry" 2>/dev/null | cut -f1)"
        rm -rf "$entry" "$entry.lock" 2>/dev/null || continue
        printf 'pruned cargo-target-bins entry %s (%sKiB freed, age %ss >= window %ss)\n' \
            "$tree" "${sz_kb:-0}" "$age" "$window"
    done < <(find "$base" -mindepth 1 -maxdepth 1 -type d -print0 2>/dev/null)
}
