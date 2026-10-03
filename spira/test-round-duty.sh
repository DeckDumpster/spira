#!/usr/bin/env bash
#
# test-round-duty.sh — a leftover rounds/<n>.running marker must not silence ROUND DUE:
# a stale marker is reported and removed, a live one still suppresses.
#
# tier: T1
# covers: spira/round-duty.sh spira/watchers spira/conf.d/SPIRA_ROUND_*
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

echo "test-round-duty.sh"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
git init -q "$TMP/repo"; git -C "$TMP/repo" -c user.name=t -c user.email=t@t commit -q --allow-empty -m x
tip="$(git -C "$TMP/repo" rev-parse HEAD)"
git -C "$TMP/repo" branch spira/sp-aaa
mkdir -p "$TMP/run/landstate" "$TMP/run/rounds"
echo "CERTIFIED $tip" > "$TMP/run/landstate/sp-aaa"

run() {
    env -i PATH="$PATH" HOME="$TMP" SPIRA_RUN="$TMP/run" SPIRA_REPO="$TMP/repo" \
        SPIRA_ROUND_MIN=1 SPIRA_ROUND_MARKER_MAX_AGE=3600 bash "$HERE/round-duty.sh" "$@" 2>&1
}

M="$TMP/run/rounds/100.running"
: > "$M"
nowant "a fresh marker suppresses ROUND DUE" "ROUND DUE" "$(run watch --interval 1 --ticks 1)"
[ -e "$M" ]; is "a fresh marker is left in place" "0" "$?"

touch -d '9 hours ago' "$M"
out="$(run watch --interval 1 --ticks 1)"
want "a stale marker is reported" "ROUND MARKER STALE: rounds/100.running" "$out"
want "ROUND DUE still fires past a stale marker" "ROUND DUE: 1 certified" "$out"
[ ! -e "$M" ]; is "the stale marker is removed" "0" "$?"

: > "$M"; echo "red" > "$TMP/run/rounds/100.result"; touch -d '1 minute ago' "$M"; touch "$TMP/run/rounds/100.result"
out="$(run watch --interval 1 --ticks 1)"
want "a marker its round has finished is stale" "outlived its round" "$out"
want "ROUND DUE fires after a finished round's marker" "ROUND DUE" "$out"

run watch --interval 7 --ticks 1 >/dev/null
run health >/dev/null; is "health passes after a fresh poll" "0" "$?"
echo "ok 1 100 7" > "$TMP/run/watchd/round-duty.health"
run health >/dev/null; is "health fails once the last poll is stale" "1" "$?"

grep -q '^round-duty|daemon|' "$HERE/watchers"; is "watchers manifest carries the round-duty row" "0" "$?"
tl_summary
