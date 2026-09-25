#!/usr/bin/env bash
#
# test-aeon-lease.sh — liveness lease (renew on trace growth, lapse on silence) and the
#   thrash wall (trip only when the deliverable fuse AND the session's own age both clear
#   SPIRA_THRASH_MINUTES), both decided by the single extracted `hb_tick`.
#
#   ./test-aeon-lease.sh
#
# MERGED FROM test-thrash-wall.sh (sp-eq8a4.2.2): the lease and the thrash wall used to be
# two copies of aeon.sh's heartbeat subshell, one per suite, each re-deriving the same three
# `if`s the tick actually runs. `hb_tick` in lib.sh is now the one thing under test in both
# halves below — these suites called their own inline copies before, which proved the copy
# self-consistent, never the aeon (Ryan's review named this directly).
#
# No database, no network, under a second.
#
# defect: sp-9ix, sp-sv34w
# covers: spira/lib.sh spira/aeon.sh spira/cockpit.sh cockpit/health.sh
# scar: the STALL_BEATS/model_idle apparatus was replaced by a liveness lease on trace growth; suites covering the old mechanism were testing code that no longer ran.
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

# Helper: run aeon_lease_minutes in a clean environment.
# Optional third arg: a pinned unix timestamp passed as SPIRA_NOW to fix the clock.
alm() {
    local bead="$1" run_dir="$2" now_arg="${3:-}"
    env -i PATH="$PATH" HOME="$TMP" LC_ALL=C.UTF-8 \
        SPIRA_CONF="$TMP/no.conf" SPIRA_RUN="$run_dir" \
        ${now_arg:+SPIRA_NOW="$now_arg"} \
        bash -c '. "$1"/lib.sh; aeon_lease_minutes "$2"' _ "$HERE" "$bead" 2>/dev/null
}

# hbt <prev_mtime> <cur_mtime> <now> <deadline> <fuse> <wall> <session_start> -> hb_tick's
# real output, called through the same lib.sh aeon.sh sources — never a copy.
hbt() {
    env -i PATH="$PATH" HOME="$TMP" LC_ALL=C.UTF-8 \
        SPIRA_CONF="$TMP/no.conf" SPIRA_RUN="$TMP/run" \
        bash -c '. "$1"/lib.sh; hb_tick "$2" "$3" "$4" "$5" "$6" "$7" "$8"' \
        _ "$HERE" "$@" 2>/dev/null
}

echo "aeon_lease_minutes — deadline file is the single source for the pane"

RUN="$TMP/run"
mkdir -p "$RUN/aeon"
BEAD="sp-test01"

# Case 1: no lease file → renders ?
result="$(alm "$BEAD" "$RUN")"
is "no lease file renders ?" "?" "$result"

# Case 2: a future deadline → positive countdown
# Pin now so deadline - now = 600 exactly, regardless of subshell timing.
pinned_now=$(date +%s)
future=$(( pinned_now + 600 ))
printf '%s' "$future" > "$RUN/aeon/$BEAD.lease"
result="$(alm "$BEAD" "$RUN" "$pinned_now")"
is "a 600s future deadline renders ~10m" "10" "$result"

# Case 3: a past deadline → negative (expired)
past=$(( $(date +%s) - 120 ))
printf '%s' "$past" > "$RUN/aeon/$BEAD.lease"
result="$(alm "$BEAD" "$RUN")"
# -2 (120 seconds past / 60 = 2 minutes expired)
is "a past deadline renders negative minutes" "-2" "$result"

# Case 4: an empty file → renders ?
: > "$RUN/aeon/$BEAD.lease"
result="$(alm "$BEAD" "$RUN")"
is "an empty lease file renders ?" "?" "$result"

# Case 5: a non-numeric file → renders ?
printf 'not-a-number' > "$RUN/aeon/$BEAD.lease"
result="$(alm "$BEAD" "$RUN")"
is "a non-numeric lease file renders ?" "?" "$result"

# Case 6: empty bead id → renders ?
result="$(alm "" "$RUN")"
is "an empty bead id renders ?" "?" "$result"

echo
echo "hb_tick — trace growth always renews, regardless of deadline or fuse"

# Growth wins even when the deadline has already passed and the fuse/wall would otherwise
# trip: aeon.sh's own loop always extends the deadline before either other check can see the
# old one, so a tick that grew can never also lapse or thrash in the same pass.
is "grew, deadline already past, fuse past wall too → still renew" "renew" \
    "$(hbt 100 200 1000 500 99 20 0)"
is "grew, everything otherwise idle → renew" "renew" "$(hbt 100 200 1000 2000 0 20 0)"

echo
echo "hb_tick — silence past the deadline lapses, before the thrash wall is even asked"

is "unchanged trace, deadline in the future → ok" "ok" "$(hbt 100 100 1000 2000 0 20 0)"
is "unchanged trace, now at the deadline → lapse" "lapse" "$(hbt 100 100 2000 2000 0 20 0)"
is "unchanged trace, now past the deadline → lapse" "lapse" "$(hbt 100 100 2001 2000 0 20 0)"
# Lapse takes priority over thrash: both conditions hold, and the tick must report the one
# that gets the session killed with the right marker (.lapsed, not .thrash).
is "silent AND stalled past the wall, but deadline already past → lapse wins" "lapse" \
    "$(hbt 100 100 90000 2000 43 20 0)"

echo
echo "hb_tick — thrash wall trips only when the fuse AND the session's own age clear it"
# sp-sv34w: aeon_fuse_minutes is a repository fact whose clock started before this session,
# so a bead whose worktree went idle before it was reclaimed reads past the wall the moment
# it is claimed. The fix requires BOTH the fuse and the session's own elapsed time to clear
# the wall before a trip fires — a session 0 minutes old cannot have stalled for 43 minutes.

# session_start pinned so (now - session_start)/60 gives an exact minute count.
is "old fuse (43m) + fresh session (0m) → ok, no trip"            "ok" "$(hbt 5 5 1000 999999999 43 20 1000)"
is "old fuse (43m) + session just under the wall (19m) → ok"      "ok" "$(hbt 5 5 $((1000+19*60)) 999999999 43 20 1000)"
is "fuse at the wall (20m) + session just under (19m) → ok"       "ok" "$(hbt 5 5 $((1000+19*60)) 999999999 20 20 1000)"

is "old fuse (43m) + session past the wall (21m) → thrash"        "thrash" "$(hbt 5 5 $((1000+21*60)) 999999999 43 20 1000)"
is "fuse at the wall + session at the wall (both 20m) → thrash"    "thrash" "$(hbt 5 5 $((1000+20*60)) 999999999 20 20 1000)"
is "large fuse + large session → thrash"                          "thrash" "$(hbt 5 5 $((1000+60*60)) 999999999 82 20 1000)"

is "fuse=? (probe failed) + old session → ok, never trips"        "ok" "$(hbt 5 5 $((1000+30*60)) 999999999 "?" 20 1000)"
is "fuse below the wall (5m) + old session (30m) → ok"            "ok" "$(hbt 5 5 $((1000+30*60)) 999999999 5 20 1000)"
is "fuse=0 + old session (30m) → ok"                              "ok" "$(hbt 5 5 $((1000+30*60)) 999999999 0 20 1000)"

echo
echo "THE KEY REGRESSION, STRUCTURALLY GONE: hb_tick never sees trace content"
# sp-9ix: the old model_idle detector read elapsed_time_seconds on the trailing heartbeat, so
# a trailing `result` line (a turn boundary) classified as elapsed=0 and "acting", and a
# session that then hung reported "acting" forever. hb_tick takes only mtimes — it has no
# argument through which trailing JSON could reach it, so that whole regression class is not
# merely fixed but unrepresentable in its inputs. An unchanged mtime past the deadline lapses
# no matter what the trace's last line says.
is "unchanged mtime past deadline lapses regardless of trace content (no such input exists)" \
    "lapse" "$(hbt 42 42 100000 99999 0 20 0)"

tl_summary
