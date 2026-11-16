#!/usr/bin/env bash
# tier: T3
# requires: testenv
# covers: spira/sim/src/pilgrim.rs spira/sim/src/world.rs spira/sim/fixtures/pilgrim-world.toml spira-lc/src/work.rs cockpit/ops/src/lcview*.rs loom/src/**
# lane: sim
# pids: 400
#
# The pilgrim world, end to end on a real throwaway store: the pilgrim files A blocked by B, lc-view
# and Loom both show A blocked; B is closed and both show A claimable. A second, plain world stands
# for production: the pilgrim is refused there and its claimable set does not move.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"
tl_config SPIRA_TEST_CASE_DEADLINE=400
[ -n "${SPIRA_RELEASE:-}" ] || bail "SPIRA_RELEASE is not set: run via testenv --with-bins"
SIM="$SPIRA_RELEASE/bin/sim"
[ -x "$SIM" ] || bail "no sim in the staged release: $SIM"
T="$(mktemp -d)"
NOLOC=(-u SPIRA_TOML -u SPIRA_RUN -u SPIRA_DB -u SPIRA_LC_PASSWORD_FILE -u SPIRA_LC_SOCKET -u SPIRA_LC_HOST -u SPIRA_LC_PORT -u SPIRA_LC_USER -u SPIRA_HOME -u SPIRA_WORK_BEAD_ID)
cleanup() {
    local f
    for f in "$T"/pilgrim/run/*.pid; do [ -f "$f" ] && kill "$(cat "$f")" 2>/dev/null; done
    for w in "$T"/pilgrim "$T"/plain; do [ -d "$w" ] && timeout 60 "$SIM" world down "$w" >/dev/null 2>&1; done # batch-job: world teardown stops a whole scratch world
    rm -rf "$T"
}
trap cleanup EXIT
trap 'exit 143' INT TERM

SIMENV=(env "${NOLOC[@]}" TMPDIR="$T" SPIRA_SIM_RELEASE="$SPIRA_RELEASE" SPIRA_IN_TESTENV=1)
repo="$T/repo"
"$SIM" world fixture "$HERE/.." "$repo" || bail "fixture repo failed"
export SPIRA_SIM_CACHE="${SPIRA_SIM_CACHE:-/tmp/sim-seed-cache}"
(cd "$repo" && timeout 280 "${SIMENV[@]}" "$SIM" world up "$T/pilgrim" --pilgrim) >"$T/up1.out" 2>&1 || bail "pilgrim world up: $(cat "$T/up1.out")" # batch-job: a whole world
(cd "$repo" && timeout 280 "${SIMENV[@]}" "$SIM" world up "$T/plain") >"$T/up2.out" 2>&1 || bail "plain world up: $(cat "$T/up2.out")" # batch-job: a whole world

in_world() { local w="$1"; shift; env "${NOLOC[@]}" "$SIM" world exec "$w" "$*"; } # batch-job: one tool call in a world
pilgrim_work() { # pilgrim_work <world> <verb and args...>
    local w="$1"; shift
    in_world "$w" "spira-lc work sp-pilgrimage $* --actor pilgrim"
}
pilgrim_file() { # pilgrim_file <world> <title>
    in_world "$1" "spira-lc work sp-pilgrimage file '$2' --for builder --repo sim --body-file - --stdin '## Build
the $2' --actor pilgrim" 2>/dev/null
}
plain() { sed 's/\x1b\[[0-9;]*m//g'; }
claimable_ids() { in_world "$1" "spira-lc list --live --json" | jq -r '[.[] | select(.claim_view.verdict == "claimable") | .bead_id] | sort | join(" ")'; }

# The matcher must be able to find something: the fixture's own bead is claimable at the start.
STANDING="$(claimable_ids "$T/pilgrim")"
is "the fixture seeds one claimable bead" 1 "$(echo "$STANDING" | wc -w)"
PROD_BEFORE="$(claimable_ids "$T/plain")"
pilgrim_file "$T/plain" "refused in production" >/dev/null 2>&1; rc=$?
wantrc "the pilgrim cannot file in a world that is not a pilgrim world" 3 "$rc"

B="$(pilgrim_file "$T/pilgrim" "blocker B")"
A="$(pilgrim_file "$T/pilgrim" "blocked A")"
want "the pilgrim files B in its own world" "sp-" "$B"
want "the pilgrim files A in its own world" "sp-" "$A"
pilgrim_work "$T/pilgrim" dep-add "$A" "$B" >/dev/null 2>&1
is "dep-add exits 0" 0 "$?"

once="$(in_world "$T/pilgrim" "lc-view once" | plain)"
want "lc-view shows A blocked on B" "$A <- $B" "$once"
want "lc-view counts the blocked bead" "BLOCKED 1" "$once"
nowant "lc-view does not offer A as claimable" "P2 $A" "$(echo "$once" | sed -n '/^NEXT/,/^BLOCKED/p')"

ADDR="$(sed -n 's/^loom_addr *= *"\(.*\)"/\1/p' "$T/pilgrim/config/sim.toml" | head -1)"
want "the world gave Loom an address of its own" "127.0.0.1:" "$ADDR"
env "${NOLOC[@]}" "$SIM" world exec "$T/pilgrim" 'echo $$ > "$SPIRA_RUN/loom.pid"; exec loom' >"$T/loom.log" 2>&1 &
env "${NOLOC[@]}" "$SIM" world exec "$T/pilgrim" 'echo $$ > "$SPIRA_RUN/lc-view.pid"; exec lc-view loop 1' >"$T/lv.log" 2>&1 &
loom_view() { curl -s -m 20 "http://$ADDR/api/lifecycle"; }
await_loom() { # await_loom <jq predicate over .view>
    local i
    for i in $(seq 1 40); do
        loom_view | jq -e "$1" >/dev/null 2>&1 && return 0
        sleep 0.5
    done
    return 1
}
await_loom "[.view.blocked[].id] | index(\"$A\") != null"
is "Loom shows A blocked" 0 "$?"
loom_view | jq -e "[.view.next[].id] | index(\"$A\") == null" >/dev/null
is "Loom does not offer A as claimable while B is open" 0 "$?"

closed="$(pilgrim_work "$T/pilgrim" close-other "$B" --evidence walk-closes-the-blocker 2>&1)"
is "close-other exits 0: $closed" 0 "$?"
once="$(in_world "$T/pilgrim" "lc-view once" | plain)"
nowant "lc-view no longer shows A blocked" "$A <- $B" "$once"
want "lc-view offers A as claimable" "P2 $A" "$(echo "$once" | sed -n '/^NEXT/,/^[A-Z]/p')"
await_loom "[.view.next[].id] | index(\"$A\") != null"
is "Loom offers A as claimable" 0 "$?"
loom_view | jq -e '.view.blocked | length == 0' >/dev/null
is "Loom shows nothing blocked" 0 "$?"

is "production's claimable set is unchanged" "$PROD_BEFORE" "$(claimable_ids "$T/plain")"
tl_summary
