#!/usr/bin/env bash
#
# test-round-duty.sh — ROUND DUE is judged from `queue round status`, never from a marker file:
# an open round suppresses it, a failed status is "unknown" and pages nothing.
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
mkdir -p "$TMP/run/rounds" "$TMP/bin"
printf '#!/usr/bin/env bash\n[ "$1 $2" = "list-state CERTIFIED" ] && printf "sp-aaa\\t\\t\\n"\nexit 0\n' > "$TMP/bin/spira-lc"
printf '#!/usr/bin/env bash\n[ "$1 $2" = "round status" ] || exit 2\ncase "$(cat QMODE)" in\n open) printf "round=open\\nbatch_id=r-1\\n" ;;\n none) echo round=none ;;\n garbage) echo hello ;;\n fail) echo boom >&2; exit 1 ;;\nesac\n' > "$TMP/bin/queue"
sed -i "s|QMODE|$TMP/qmode|" "$TMP/bin/queue"
chmod +x "$TMP/bin/spira-lc" "$TMP/bin/queue"
PATH="$TMP/bin:$PATH"

run() {
    tl_config SPIRA_RUN="$TMP/run" SPIRA_ROUND_MIN=1
    env -i PATH="$PATH" HOME="$TMP" SPIRA_TOML="$SPIRA_TOML" SPIRA_REPO="$TMP/repo" \
        bash "$HERE/round-duty.sh" "$@" 2>&1
}

echo open > "$TMP/qmode"
nowant "an open round suppresses ROUND DUE (no marker written)" "ROUND DUE" "$(run watch --interval 1 --ticks 1)"

echo none > "$TMP/qmode"
M="$TMP/run/rounds/100.running"; : > "$M"
want "no round pages ROUND DUE even past a leftover marker" "ROUND DUE: 1 certified" "$(run watch --interval 1 --ticks 1)"
rm -f "$M"

for m in fail garbage; do
    echo "$m" > "$TMP/qmode"
    out="$(run watch --interval 1 --ticks 1)"
    want "status '$m' is reported as unknown" "ROUND STATE UNKNOWN" "$out"
    nowant "status '$m' does not page ROUND DUE" "ROUND DUE" "$out"
done

echo none > "$TMP/qmode"
run watch --interval 1 --ticks 1 >/dev/null
rm -f "$TMP/run/rounds/"*; : > "$TMP/run/rounds/300.result"
(sleep 1.5; echo "red sp-aaa" >> "$TMP/run/rounds/300.result"; sleep 1; echo "round 300: no verdict from any VM" >> "$TMP/run/rounds/300.result") &
out="$(run watch --interval 1 --ticks 5)"; wait
want "a result line is announced" "ROUND RESULT 300: red sp-aaa" "$out"
want "a verdict-less result is reported as its own condition" "ROUND NO VERDICTS 300" "$out"
[ "$(printf '%s\n' "$out" | grep -c 'ROUND NO VERDICTS')" = 1 ]; is "only the verdict-less line raises it" "0" "$?"

run watch --interval 7 --ticks 1 >/dev/null
run health >/dev/null; is "health passes after a fresh poll" "0" "$?"
echo "ok 1 100 7" > "$TMP/run/watchd/round-duty.health"
run health >/dev/null; is "health fails once the last poll is stale" "1" "$?"

INBOX="$TMP/inbox.log"
echo open > "$TMP/qmode"
wrun() {
    tl_config SPIRA_RUN="$TMP/run" SPIRA_ROUND_MIN=1 \
        SPIRA_CONCIERGE_INBOX="${WINBOX:-$INBOX}"
    env -i PATH="$PATH" HOME="$TMP" SPIRA_TOML="$SPIRA_TOML" SPIRA_REPO="$TMP/repo" \
        bash "$HERE/round-duty.sh" "$@" 2>&1
}
wrun watch --interval 1 --ticks 1 >/dev/null
[ ! -s "$INBOX" ]; is "a pass with nothing due writes nothing to the inbox" "0" "$?"
echo none > "$TMP/qmode"
out="$(WINBOX="$TMP" wrun watch --interval 1 --ticks 1)"
want "a failed wake does not lose ROUND DUE from the log" "ROUND DUE" "$out"

grep -q '^round-duty|daemon|' "$HERE/watchers"; is "watchers manifest carries the round-duty row" "0" "$?"
tl_summary
