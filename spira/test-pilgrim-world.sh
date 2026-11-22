#!/usr/bin/env bash
# tier: T3
# requires: testenv
# covers: spira/sim/src/pilgrim.rs spira/sim/src/pilgrim_land.rs spira/sim/fixtures/pilgrim-tree/* spira/sim/src/world.rs spira/sim/fixtures/pilgrim-world.toml spira-lc/src/work.rs cockpit/ops/src/lcview*.rs loom/src/**
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
T="$(mktemp -d /tmp/pw.XXXXXX)" # short root: <world>/pilgrim/lc.sock must stay under the 107-byte sun_path limit
[ "${#T}" -le 60 ] || bail "scratch root $T is too long for <world>/pilgrim/lc.sock"
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
export SPIRA_SIM_CACHE="${SPIRA_SIM_CACHE:-$T/seed-cache}" # per-run: a fixed /tmp path made by another slot or user is not ours to write
(cd "$repo" && timeout 280 "${SIMENV[@]}" "$SIM" world up "$T/pilgrim" --pilgrim) >"$T/up1.out" 2>&1 || bail "pilgrim world up: $(cat "$T/up1.out") [sim=$(stat -Lc '%A %U' "$SIM" 2>&1) cache=$(stat -c '%A %U' "$SPIRA_SIM_CACHE" "$(dirname "$SPIRA_SIM_CACHE")" 2>&1 | tr '\n' ' ')]" # batch-job: a whole world
(cd "$repo" && timeout 280 "${SIMENV[@]}" "$SIM" world up "$T/plain") >"$T/up2.out" 2>&1 || bail "plain world up: $(cat "$T/up2.out")" # batch-job: a whole world

in_world() { local w="$1"; shift; env "${NOLOC[@]}" "$SIM" world exec "$w" "$*"; } # batch-job: one tool call in a world
pilgrim_work() { # pilgrim_work <world> <verb and args...>
    local w="$1"; shift
    in_world "$w" "spira-lc work sp-pilgrimage $* --actor pilgrim"
}
pilgrim_file() { # pilgrim_file <world> <title>; on an empty id or a non-zero exit, prints #-lines naming why, then the id
    local errf out rc
    errf="$(mktemp "$T/file-err.XXXXXX")"
    out="$(in_world "$1" "spira-lc work sp-pilgrimage file '$2' --for builder --repo sim --body-file - --stdin '## Build
the $2' --actor pilgrim" 2>"$errf")"
    rc=$?
    if [ -z "$out" ] || [ "$rc" -ne 0 ]; then
        {
            echo "# pilgrim_file '$2' exited $rc"
            sed 's/^/# stderr: /' "$errf"
            sed 's/^/# lc-run.log: /' "$T/pilgrim/lc-run.log" 2>&1
        } >&2
    fi
    printf '%s' "$out"
    return "$rc"
}
plain() { sed 's/\x1b\[[0-9;]*m//g'; }
claimable_ids() { in_world "$1" "spira-lc list --live --json" | jq -r '[.[] | select(.claim_view.verdict == "claimable") | .bead_id] | sort | join(" ")'; }

# The matcher must be able to find something: the fixture's own bead is claimable at the start.
STANDING="$(claimable_ids "$T/pilgrim")"
is "the fixture seeds one claimable bead" 1 "$(echo "$STANDING" | wc -w)"
PROD_BEFORE="$(claimable_ids "$T/plain")"
PROD_ROWS="$(in_world "$T/plain" "spira-lc list --live --json" | jq length)"
pilgrim_file "$T/plain" "refused in production" >/dev/null 2>&1; rc=$?
wantrc "the pilgrim cannot file in a world that is not a pilgrim world" 3 "$rc"
is "the refused filing left no bead behind" "$PROD_ROWS" "$(in_world "$T/plain" "spira-lc list --live --json" | jq length)"

B="$(pilgrim_file "$T/pilgrim" "blocker B")"
A="$(pilgrim_file "$T/pilgrim" "blocked A")"
want "the pilgrim files B in its own world" "sp-" "$B"
want "the pilgrim files A in its own world" "sp-" "$A"
pilgrim_work "$T/pilgrim" dep-add "$A" "$B" >/dev/null 2>&1
is "dep-add exits 0" 0 "$?"

# A refused claim sync stays owed to the settler (settle.rs sync_claims) while the verb exits 0: the view settles, it does not flip.
view_once_until() { # view_once_until <grep -F needle present|absent>; leaves the last render in $once
    local i
    for i in $(seq 1 40); do
        once="$(in_world "$T/pilgrim" "lc-view once" | plain)"
        if [ "$2" = present ]; then grep -qF -- "$1" <<<"$once" && return 0; else grep -qF -- "$1" <<<"$once" || return 0; fi
        sleep 0.5
    done
    return 1
}
view_once_until "$A <- $B" present
want "lc-view shows A blocked on B" "$A <- $B" "$once"
want "lc-view counts the blocked bead" "BLOCKED 1" "$once"
nowant "lc-view does not offer A as claimable" "P2 $A" "$(echo "$once" | sed -n '/^NEXT/,/^BLOCKED/p')"

ADDR="$(sed -n 's/^loom_addr *= *"\(.*\)"/\1/p' "$T/pilgrim/config/sim.toml" | head -1)"
want "the world gave Loom an address of its own" "127.0.0.1:" "$ADDR"
env "${NOLOC[@]}" "$SIM" world exec "$T/pilgrim" 'echo $$ > "$SPIRA_RUN/loom.pid"; exec loom' >"$T/loom.log" 2>&1 &
env "${NOLOC[@]}" "$SIM" world exec "$T/pilgrim" 'echo $$ > "$SPIRA_RUN/lc-view.pid"; exec lc-view loop 1' >"$T/lv.log" 2>&1 &
loom_view() { curl -s -m 5 "http://$ADDR/api/lifecycle"; }
await_loom() { # await_loom <jq predicate over .view>
    local i
    for i in $(seq 1 40); do
        loom_view | jq -e "$1" >/dev/null 2>&1 && return 0
        sleep 0.5
    done
    return 1
}
await_loom "[.view.blocked[].id] | index(\"$A\") != null"
loom_rc=$?
[ "$loom_rc" -eq 0 ] || { echo "# loom.log: $(head -c 600 "$T/loom.log")"; echo "# loom api: $(curl -s -m 5 -i "http://$ADDR/api/lifecycle" | head -c 600)"; }
is "Loom shows A blocked" 0 "$loom_rc"
loom_view | jq -e "[.view.next[].id] | index(\"$A\") == null" >/dev/null
is "Loom does not offer A as claimable while B is open" 0 "$?"

close_once() { pilgrim_work "$T/pilgrim" close-other "$B" --evidence walk-closes-the-blocker 2>&1; }
# A client waits a bounded time for the service's answer and a late one is "cannot tell"; groomer close is a no-op on a closed bead, so ask again and say so.
for attempt in 1 2 3; do
    closed="$(close_once)"
    close_rc=$?
    [ "$close_rc" -eq 0 ] && break
    echo "# close-other attempt $attempt exit $close_rc: $closed"
    echo "# lc-serve.log: $(tail -c 1500 "$T/pilgrim/lc-serve.log")"
    [[ "$closed" == *"cannot tell"* ]] || break
done
is "close-other exits 0: $closed" 0 "$close_rc"
view_once_until "$A <- $B" absent
nowant "lc-view no longer shows A blocked" "$A <- $B" "$once"
want "lc-view offers A as claimable" "P2 $A" "$(echo "$once" | sed -n '/^NEXT/,/^[A-Z]/p')"
await_loom "[.view.next[].id] | index(\"$A\") != null"
is "Loom offers A as claimable" 0 "$?"
loom_view | jq -e '.view.blocked | length == 0' >/dev/null
is "Loom shows nothing blocked" 0 "$?"

# The rune's documented prefix, from a session whose own SPIRA_TOML names the other store: the registry beside the socket decides, the socket alone does not.
session_rows() { env "${NOLOC[@]}" SPIRA_HOME="$SPIRA_RELEASE/spira" SPIRA_TOML="$T/plain/config/sim.toml" "$@" "$SPIRA_RELEASE/bin/spira-lc" list --live --json | jq length; }
WORLD_ROWS="$(in_world "$T/pilgrim" "spira-lc list --live --json" | jq length)"
[ "$WORLD_ROWS" != "$PROD_ROWS" ]
is "the two worlds hold different stores" 0 "$?"
is "the session's registry alone reads its own store" "$PROD_ROWS" "$(session_rows)"
is "the rune's prefix reads the pilgrim world's store" "$WORLD_ROWS" "$(session_rows SPIRA_TOML="$T/pilgrim/config/sim.toml" SPIRA_LC_SOCKET="$T/pilgrim/lc.sock")"
is "the socket alone is overridden by the session's registry" "$PROD_ROWS" "$(session_rows SPIRA_LC_SOCKET="$T/pilgrim/lc.sock")"

is "production's claimable set is unchanged" "$PROD_BEFORE" "$(claimable_ids "$T/plain")"
tl_summary
