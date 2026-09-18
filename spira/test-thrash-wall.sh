#!/usr/bin/env bash
#
# test-thrash-wall.sh — thrash wall floors the fuse at session elapsed time.
#
#   ./test-thrash-wall.sh
#
# The thrash wall trips an aeon that renews its lease while its deliverable
# does not move. The defect (sp-requeue-thrash): aeon_fuse_minutes is a
# repository fact whose clock started before this session, so a bead whose
# worktree went idle before it was reclaimed is past the wall the moment it
# is claimed — killed 31 seconds in with "no last action".
#
# The fix: trip only when BOTH the fuse exceeds the wall AND the session's
# own elapsed time exceeds the wall. A session 31 seconds old cannot have
# stalled for 43 minutes.
#
# No database, no network, under a second.
#
# defect: sp-sv34w
# covers: spira/aeon.sh spira/lib.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

is() {
    if [ "$2" = "$3" ]; then
        pass=$((pass+1)); printf '  ok    %s\n' "$1"
    else
        fail=$((fail+1)); printf '  FAIL  %s: want [%s] got [%s]\n' "$1" "$2" "$3"
    fi
}

# thrash_check <fuse_m> <wall_m> <session_m> -> "trip" or "ok"
# Reproduces the trip condition in aeon.sh's heartbeat subshell (post sp-sv34w).
# Both fuse and session_elapsed must exceed the wall for a trip to fire.
thrash_check() {
    local fuse_m="$1" wall_m="$2" session_m="$3"
    if [[ "${fuse_m:-?}" =~ ^[0-9]+$ ]] && [ "$fuse_m" -ge "$wall_m" ] && [ "$session_m" -ge "$wall_m" ] 2>/dev/null; then
        printf 'trip'
    else
        printf 'ok'
    fi
}

echo "aeon_fuse_minutes — backdated worktree reads past the wall (positive control)"
# Positive control: proves the probe finds the defect class. A worktree whose
# newest mtime is far in the past must read a fuse well above the wall. Without
# this, a passing 'no trip' assertion could be explained by the probe returning
# '?' rather than a large number.

WT="$TMP/worktree/sp-twtest"
mkdir -p "$WT"
touch -d "2 hours ago" "$WT/README"
# The directory mtime updates when the file is created; backdate it too so
# find's max mtime is the file's, not the freshly-created directory's.
touch -d "2 hours ago" "$WT"

fuse_out="$(env -i PATH="$PATH" HOME="$TMP" LC_ALL=C.UTF-8 \
    SPIRA_CONF="$TMP/no.conf" SPIRA_RUN="$TMP/run" SPIRA_HOME="$HERE" \
    bash -c '. "$1"/lib.sh; aeon_fuse_minutes "sp-twtest" "$2" ""' \
    _ "$HERE" "$WT" 2>/dev/null)"

if [[ "$fuse_out" =~ ^[0-9]+$ ]] && [ "$fuse_out" -ge 20 ] 2>/dev/null; then
    pass=$((pass+1))
    printf '  ok    backdated worktree reads fuse=%sm (>= 20m wall)\n' "$fuse_out"
else
    fail=$((fail+1))
    printf '  FAIL  backdated worktree should read >= 20m, got [%s]\n' "$fuse_out"
fi

echo
echo "thrash_check — session floor prevents trip on fresh session"
# THE DEFECT: a fresh session sees the old fuse and trips immediately.
# After the fix, the session elapsed guard blocks the trip.

is "old fuse (43m) + fresh session (0m) → no trip"     "ok" "$(thrash_check 43 20 0)"
is "old fuse (43m) + session just under wall (19m) → no trip" \
                                                         "ok" "$(thrash_check 43 20 19)"
is "fuse at wall (20m) + session just under wall (19m) → no trip" \
                                                         "ok" "$(thrash_check 20 20 19)"

echo
echo "thrash_check — genuine stall still trips"
# A session that has been alive past the wall AND whose deliverable has not
# moved for at least that long must still be killed.

is "old fuse (43m) + session past wall (21m) → trip"   "trip" "$(thrash_check 43 20 21)"
is "fuse at wall (20m) + session at wall (20m) → trip"  "trip" "$(thrash_check 20 20 20)"
is "large fuse + large session → trip"                   "trip" "$(thrash_check 82 20 60)"

echo
echo "thrash_check — probe failures and recent fuse are always safe"

is "fuse=? (probe failed) + old session → no trip"  "ok" "$(thrash_check "?" 20 30)"
is "fuse < wall (5m) + old session (30m) → no trip" "ok" "$(thrash_check 5 20 30)"
is "fuse=0 + old session (30m) → no trip"           "ok" "$(thrash_check 0 20 30)"

echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
