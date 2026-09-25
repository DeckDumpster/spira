#!/usr/bin/env bash
#
# test-thrash-streak.sh — a thrash requeue exempts one attempt, not an unbounded run of them
# at the same commit (sp-4rzlw).
#
# THE DEFECT THIS GUARDS. attempts_of subtracts every requeued/thrash event with no bound, so
# a bead that keeps getting killed by the deliverable-progress wall at the exact same branch
# tip is exempted forever — the attempt cap and the poison threshold never get a chance to
# trip. sp-gs24i got five summons across seven hours after its ejection and none of them
# charged.
#
# thrash_streak_bump (lib.sh) compares the branch tip at each thrash to the tip recorded at
# the last one on the same bead (bead metadata — survives across summons, unlike $SPIRA_RUN).
# The same tip bumps the streak; any other tip, including a bead's first-ever thrash, resets
# it to 1. At or over SPIRA_THRASH_STREAK_CAP consecutive same-tip thrashes, aeon.sh switches
# the requeue cause from the exempt "thrash" to "thrash-stale", which attempts_of does not
# subtract — so the claim that preceded it counts, same as any other failed session.
#
# A REAL bd ON A THROWAWAY DATABASE: thrash_streak_bump writes and reads bead metadata via
# `bd update --set-metadata` / `bd show --long`, and attempts_of reads the events table via
# `bd sql` — both real bd behaviors a stub would have to reimplement rather than exercise
# (law-prefer-the-real-dependency).
#
# covers: spira/lib.sh spira/aeon.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()  { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }

TMP="$(mktemp -d)"
# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-thrash-streak
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
# testdb-mode: server — thrash_streak_bump reads/writes bead metadata via `bd update
# --set-metadata` / `bd show --long`, and attempts_of reads the events table via `bd sql`;
# both are refused against the embedded engine.
export SPIRA_TESTDB_MODE=server
testdb_up thrash-streak || {
    printf 'SKIP test-thrash-streak: server testdb not available\n' >&2
    exit 77
}
# shellcheck disable=SC1090
. "$HERE/lib.sh"

seed() {   # seed <id> — one open, claimable bead
    testdb_reset
    testdb_seed <<JSONL
{"id":"$1","title":"a bead","status":"open","issue_type":"task","labels":["spira","plan"],"updated_at":"2026-09-24T00:00:00Z"}
JSONL
}
num() { local v="$1"; printf '%d' "${v:-0}"; }
poisons() { local n; n="$(num "$(attempts_of "$1")")"; [ "$n" -ge 3 ] && echo yes || echo no; }

# cycle_thrash <id> <tip> <cause> — the exact shape aeon.sh's cleanup writes for a thrash
# requeue: the in_progress claim from the aeon that got killed, then the requeued event.
cycle_thrash() {
    local id="$1" cause="$3" uuid
    bdq update "$id" --status in_progress >/dev/null 2>&1
    uuid="$(python3 -c 'import uuid; print(str(uuid.uuid4()))')"
    bdq sql "INSERT INTO events (id, issue_id, event_type, actor, new_value, created_at) VALUES ('$uuid', '$id', 'requeued', 'harness', '$cause', NOW())" >/dev/null 2>&1
    bdq update "$id" --status open >/dev/null 2>&1
}

echo "thrash_streak_bump: streak accounting against a real bead"
echo

seed sp-ts1
# POSITIVE CONTROL: a bead that has never thrashed carries no thrash_streak metadata at all.
is "fresh bead carries no thrash_streak"          "" "$(bead_metadata sp-ts1 thrash_streak)"

is "first thrash at tip aaa111 is streak 1"        1 "$(thrash_streak_bump sp-ts1 aaa111 "stuck on X")"
is "metadata records the tip"                 aaa111 "$(bead_metadata sp-ts1 thrash_tip)"
is "metadata records the sticking point"  "stuck on X" "$(bead_metadata sp-ts1 thrash_last)"

is "second thrash, SAME tip, is streak 2"          2 "$(thrash_streak_bump sp-ts1 aaa111 "still stuck on X")"
is "a moved tip resets the streak to 1"            1 "$(thrash_streak_bump sp-ts1 bbb222 "new problem")"
is "next thrash back at that new tip is streak 2"  2 "$(thrash_streak_bump sp-ts1 bbb222 "still new problem")"

echo
echo "T1 — 2 consecutive thrash requeues with an unchanged branch tip -> attempt charged:"
echo

seed sp-ts2
is "a fresh bead has no attempts"                              0 "$(num "$(attempts_of sp-ts2)")"

# Streak 1 at tip aaa — below SPIRA_THRASH_STREAK_CAP (default 2) — still exempt.
streak="$(thrash_streak_bump sp-ts2 aaa "stuck")"
is "first thrash streak is 1 (below cap)"                      1 "$streak"
cycle_thrash sp-ts2 aaa thrash
is "streak 1 (below cap) requeue stays net-zero"               0 "$(num "$(attempts_of sp-ts2)")"

# Streak 2 at the SAME tip aaa — at cap — the requeue is charged this time.
streak="$(thrash_streak_bump sp-ts2 aaa "still stuck")"
is "second thrash at the same tip reaches the cap"             2 "$streak"
cycle_thrash sp-ts2 aaa thrash-stale
is "2 consecutive thrashes, unchanged tip -> attempt charged"  1 "$(num "$(attempts_of sp-ts2)")"

echo
echo "T1 — a thrash followed by a new commit resets the counter:"
echo

seed sp-ts3
thrash_streak_bump sp-ts3 aaa "stuck" >/dev/null
cycle_thrash sp-ts3 aaa thrash
# A commit lands — the tip moves to bbb — before the next thrash.
streak="$(thrash_streak_bump sp-ts3 bbb "different sticking point")"
is "a moved tip resets the streak to 1"                        1 "$streak"
cycle_thrash sp-ts3 bbb thrash
is "reset streak stays net-zero even after 2 thrashes total"   0 "$(num "$(attempts_of sp-ts3)")"

echo
echo "T1 — a poison threshold reached via thrash:"
echo

seed sp-ts4
tip=aaa
# Poisoning needs THREE charged attempts (spira/conf.sh). The first same-tip thrash stays
# exempt (streak 1, below SPIRA_THRASH_STREAK_CAP=2); every one after it is charged, so
# reaching poison via thrash alone takes 1 exempt + 3 charged = 4 consecutive same-tip
# thrashes, not 3.
for i in 1 2 3 4; do
    streak="$(thrash_streak_bump sp-ts4 "$tip" "still stuck, round $i")"
    if [ "$streak" -ge 2 ]; then
        cycle_thrash sp-ts4 "$tip" thrash-stale
    else
        cycle_thrash sp-ts4 "$tip" thrash
    fi
done
is "four same-tip thrashes (1 exempt + 3 charged) reach the poison threshold" \
   yes "$(poisons sp-ts4)"

echo
echo "$pass passed, $fail failed"
[ "$fail" = 0 ]
