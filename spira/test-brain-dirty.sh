#!/usr/bin/env bash
# test-brain-dirty.sh — a wiki path uncommitted past SPIRA_BRAIN_DIRTY_AGE is alarmed once;
# a fresh one, a clean tree and a committed one are quiet.
#
# tier: T1
# covers: spira/brain-dirty.sh spira/watchers spira/conf.d/SPIRA_BRAIN_DIRTY_AGE
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

echo "test-brain-dirty.sh"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
G="$HERE/brain-dirty.sh"
BRAIN="$TMP/brain"; git init -q "$BRAIN"; mkdir -p "$BRAIN/wiki/notes" "$TMP/run"
echo seed > "$BRAIN/wiki/notes/seed.md"
git -C "$BRAIN" add -A; git -C "$BRAIN" -c user.name=t -c user.email=t@t commit -qm init
INBOX="$TMP/inbox.log"
tl_config SPIRA_WIKI="$BRAIN" SPIRA_RUN="$TMP/run" SPIRA_CONCIERGE_INBOX="$INBOX" SPIRA_BRAIN_DIRTY_AGE=1800
w() { env -i PATH="$PATH" HOME="$TMP" SPIRA_TOML="$SPIRA_TOML" bash "$G" "$@" 2>&1; }

is "a clean tree is quiet" "" "$(w pass)"

echo fresh > "$BRAIN/wiki/notes/fresh.md"
is "a page dirty for seconds is quiet" "" "$(w pass)"

echo old > "$BRAIN/wiki/notes/old.md"
touch -d '2 hours ago' "$BRAIN/wiki/notes/old.md"
out="$(w pass)"
want "a page dirty past the age is alarmed" "BRAIN DIRTY: wiki/notes/old.md" "$out"
nowant "the fresh page is not named" "fresh.md" "$out"
want "the inbox gets it" "[watch:brain-dirty] BRAIN DIRTY: wiki/notes/old.md" "$(cat "$INBOX" 2>/dev/null)"
is "the same page is alarmed once" "" "$(w pass)"

echo edited >> "$BRAIN/wiki/notes/seed.md"
touch -d '3 hours ago' "$BRAIN/wiki/notes/seed.md"
want "an edit to a tracked page is judged too" "wiki/notes/seed.md" "$(w pass)"

git -C "$BRAIN" add -A; git -C "$BRAIN" -c user.name=t -c user.email=t@t commit -qm done
rm -f "$TMP/run/watchd/brain-dirty.said"
is "a committed tree is quiet" "" "$(w pass)"

w health >/dev/null; wantrc "health fails before any poll" 1 "$?"
w watch --interval 5 --ticks 1 >/dev/null
w health >/dev/null; wantrc "health passes after a poll" 0 "$?"

grep -q '^brain-dirty|' "$HERE/watchers"; is "watchers manifest has no brain-dirty row: spira-runtime hosts it" "1" "$?"

tl_summary
