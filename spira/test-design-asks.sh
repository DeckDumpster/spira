#!/usr/bin/env bash
# test-design-asks.sh — a draft or in-review design page with no open ask naming it reaches the inbox
# once a day; one with an ask, a published one and a read failure are told apart.
#
# tier: T1
# covers: spira/design-asks.sh spira/watchers spira/conf.d/SPIRA_DESIGN_ASKS_REPEAT
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

echo "test-design-asks.sh"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
G="$HERE/design-asks.sh"
BRAIN="$TMP/brain"; D="$BRAIN/wiki/projects/zed/designs"; mkdir -p "$D" "$TMP/run" "$TMP/bin"
INBOX="$TMP/inbox.log"
design() { printf -- '---\nstatus: %s\nbead: %s\n---\n# x\n' "$2" "$3" > "$D/$1.md"; }
design unasked draft sp-writer1
design covered in-review sp-writer2
design done approved sp-writer3
ASKS="$TMP/asks.json"
printf '[{"id":"sp-a","title":"approve","description":"Please approve wiki/projects/zed/designs/covered.md","notes":""}]' > "$ASKS"
cat > "$TMP/bin/bd" <<'BD'
#!/usr/bin/env bash
[ -f "$FAKE_BD_FAIL" ] && exit 1
cat "$FAKE_ASKS"
BD
chmod +x "$TMP/bin/bd"
tl_config SPIRA_WIKI="$BRAIN" SPIRA_RUN="$TMP/run" SPIRA_CONCIERGE_INBOX="$INBOX" SPIRA_DESIGN_ASKS_REPEAT=7200 SPIRA_BD="$TMP/bin/bd"
w() { env -i PATH="$PATH" ${SPIRA_RELEASE:+SPIRA_RELEASE="$SPIRA_RELEASE"} HOME="$TMP" SPIRA_TOML="$SPIRA_TOML" FAKE_ASKS="$ASKS" \
        FAKE_BD_FAIL="$TMP/bdfail" "$@" 2>&1; }

out="$(w bash "$G" pass)"
want "a draft design with no ask is told" "wiki/projects/zed/designs/unasked.md is draft" "$out"
want "the line names its writer" "sp-writer1" "$out"
want "the line says how long" "no open ask for 0h" "$out"
nowant "a design with an open ask is quiet" "covered.md" "$out"
nowant "an approved design is quiet" "done.md" "$out"
want "the inbox gets it" "[watch:design-asks] DESIGN WITHOUT ASK" "$(cat "$INBOX" 2>/dev/null)"
is "within the repeat interval it is not repeated" "" "$(w bash "$G" pass)"

awk '{ $2 -= 10800; $3 -= 10800; print }' "$TMP/run/watchd/design-asks.state" > "$TMP/s" && mv "$TMP/s" "$TMP/run/watchd/design-asks.state"
want "past the repeat interval it is told again, with its age" "no open ask for 3h" "$(w bash "$G" pass)"

printf '[{"id":"sp-b","title":"t","description":"wiki/projects/zed/designs/unasked.md wiki/projects/zed/designs/covered.md","notes":""}]' > "$ASKS"
is "an ask naming the path silences it" "" "$(w bash "$G" pass)"
design fresh draft sp-writer4
want "a new unasked page is told" "fresh.md" "$(w bash "$G" pass)"

w bash "$G" health >/dev/null; wantrc "health fails before any poll" 1 "$?"
w bash "$G" watch --interval 5 --ticks 1 >/dev/null
w bash "$G" health >/dev/null; wantrc "health passes after a good poll" 0 "$?"

: > "$TMP/bdfail"
out="$(w bash "$G" pass)"; wantrc "an unreadable store fails the pass" 1 "$?"
want "an unreadable store is said as a failure" "DESIGN-ASKS FAILED" "$out"
nowant "an unreadable store is not read as no designs" "DESIGN WITHOUT ASK" "$out"
is "the same failure is not repeated" "" "$(w bash "$G" pass)"
w bash "$G" health >/dev/null; wantrc "health fails after a failed pass" 1 "$?"
rm -f "$TMP/bdfail"

rm -f "$TMP/run/watchd/design-asks.state" "$INBOX"
printf -- '---\nstatus: draft\nbead: sp-w\n---\n# Alpha\nhttps://claude.ai/artifact/AAAA1111\n' > "$D/alpha.md"
printf -- '---\nstatus: draft\nbead: sp-w\n---\n# Beta\nhttps://claude.ai/artifact/BBBB2222\n' > "$D/beta.md"
rm -f "$D/unasked.md" "$D/covered.md" "$D/fresh.md" "$D/done.md"
printf '[{"id":"sp-c","title":"t","description":"see https://claude.ai/artifact/AAAA1111 and nothing else","notes":""}]' > "$ASKS"
out="$(w bash "$G" pass)"
nowant "an ask carrying only the phone-copy URL clears the alarm" "alpha.md" "$out"
want "an ask naming another design's URL does not clear it" "beta.md" "$out"
printf '[{"id":"sp-c","title":"t","description":"approve beta please","notes":""}]' > "$ASKS"
rm -f "$TMP/run/watchd/design-asks.state"
out="$(w bash "$G" pass)"
nowant "an ask carrying only the slug clears the alarm" "beta.md" "$out"
want "an ask naming neither still alarms" "alpha.md" "$out"

rm -rf "$BRAIN/wiki/projects"
out="$(w bash "$G" pass)"; wantrc "a missing brain path fails the pass" 1 "$?"
want "a missing brain path is said as a failure" "is not a directory" "$out"

tl_summary
