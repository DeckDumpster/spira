#!/usr/bin/env bash
#
# test-warm-full.sh — the warm-tmpfs watcher says the crossing past 85% once, with per-slot sizes,
# says it again after falling under, and is silent while the base is unset.
#
# tier: T1
# covers: spira/warm-full.sh spira/watchers aeon/src/run.rs spira-config/src/build.rs
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

echo "test-warm-full.sh"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
mkdir -p "$TMP/run/watchd" "$TMP/stub" "$TMP/warm/slot.1" "$TMP/warm/slot.2"
INBOX="$TMP/inbox.log"
head -c 2097152 /dev/zero > "$TMP/warm/slot.1/a"
printf '#!/usr/bin/env bash\necho "Use%%"; echo " $(cat %s/pct)%%"\n' "$TMP" > "$TMP/stub/df"
chmod +x "$TMP/stub/df"
wf() {
    local base="$1"; shift
    tl_config SPIRA_RUN="$TMP/run" SPIRA_CONCIERGE_INBOX="$INBOX" SPIRA_AEON_WARM_BASE="$base"
    env -i PATH="$TMP/stub:$PATH" HOME="$TMP" SPIRA_TOML="$SPIRA_TOML" bash "$HERE/warm-full.sh" "$@" 2>&1
}

echo 90 > "$TMP/pct"
is "silent while the base is unset" "" "$(wf '' pass)"
want "past 85% it says so with each slot's size" "slot.1=2MiB" "$(wf "$TMP/warm" pass)"
want "the line reaches the inbox" "watch:warm-full" "$(cat "$INBOX")"
is "the same crossing is said once" "" "$(wf "$TMP/warm" pass)"
echo 40 > "$TMP/pct"
is "under the limit is quiet" "" "$(wf "$TMP/warm" pass)"
echo 90 > "$TMP/pct"
want "a crossing after falling under is said again" "90% full" "$(wf "$TMP/warm" pass)"
echo 84 > "$TMP/pct"; rm -f "$TMP/run/watchd/warm-full.said"
is "84% is under the limit" "" "$(wf "$TMP/warm" pass)"

echo 40 > "$TMP/pct"; rm -f "$TMP/run/watchd/warm-full.said"
mkdir -p "$TMP/warm2/slot.1" "$TMP/warm2/slot.2"
head -c 3145728 /dev/zero > "$TMP/warm2/slot.1/a"
head -c 3145728 /dev/zero > "$TMP/warm2/slot.2/a"
exec 8>>"$TMP/warm2/slot.2.lock"; flock -n 8
wfc() { tl_config SPIRA_RUN="$TMP/run" SPIRA_CONCIERGE_INBOX="$INBOX" SPIRA_AEON_WARM_BASE="$TMP/warm2"
    env -i PATH="$TMP/stub:$PATH" HOME="$TMP" SPIRA_TOML="$SPIRA_TOML" SPIRA_AEON_WARM_CAP_MIB=2 bash "$HERE/warm-full.sh" pass 2>&1; }
want "a pass reports emptying an idle slot over the cap" "slot.1 emptied" "$(wfc)"
is "the idle slot is empty after one pass" "0" "$(find "$TMP/warm2/slot.1" -type f | wc -l | tr -d ' ')"
is "a held slot is left alone" "1" "$(find "$TMP/warm2/slot.2" -type f | wc -l | tr -d ' ')"
printf 'sp-held1' > "$TMP/warm2/slot.2.lock"
echo 90 > "$TMP/pct"; rm -f "$TMP/run/watchd/warm-full.said"
want "the alarm names the bead holding a slot" "slot.2=sp-held1" "$(wfc)"
exec 8>&-
