#!/usr/bin/env bash
#
# test-round-spill-key.sh — the round-spill key watcher warns once, 14 days (or the configured
# window) before the recorded expiry, again on expiry, says when no expiry is recorded, and is
# silent while spill is off.
#
# tier: T1
# covers: spira/round-spill-key.sh spira/watchers spira/conf.d/SPIRA_ROUND_VM_*
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

echo "test-round-spill-key.sh"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
mkdir -p "$TMP/run/watchd"
INBOX="$TMP/inbox.log"; KEY="$TMP/k.tskey"
: > "$KEY"

rsk() {
    local spill="$1"; shift
    tl_config SPIRA_RUN="$TMP/run" SPIRA_CONCIERGE_INBOX="$INBOX" SPIRA_ROUND_VM_SPILL="$spill" \
        SPIRA_ROUND_VM_EC2_TAILSCALE_KEY_FILE="$KEY" SPIRA_ROUND_VM_EC2_KEY_WARN_DAYS=9
    env -i PATH="$PATH" HOME="$TMP" SPIRA_TOML="$SPIRA_TOML" bash "$HERE/round-spill-key.sh" "$@" 2>&1
}
fresh() { rm -f "$TMP/run/watchd/round-spill-key.said" "$INBOX"; }

now="$(date +%s)"
echo "$((now + 5 * 86400))" > "$KEY.expires"
fresh
is "off while spill is unset" "" "$(rsk '' pass)"
nowant "a planted near expiry is not reported while spill is off" "ROUND SPILL KEY" "$(cat "$INBOX" 2>/dev/null)"

out="$(rsk ec2 pass)"
want "inside the window it warns with the days left" "expires in 4 days" "$out"
want "the warning reaches the inbox" "[watch:round-spill-key] ROUND SPILL KEY" "$(cat "$INBOX")"
is "the same condition is said once" "" "$(rsk ec2 pass)"

echo "$((now + 30 * 86400))" > "$KEY.expires"; fresh
is "outside the window it is quiet" "" "$(rsk ec2 pass)"
echo "$((now + 12 * 86400))" > "$KEY.expires"
is "the window is the configured one, not 14 days" "" "$(rsk ec2 pass)"

echo "$((now - 60))" > "$KEY.expires"
want "an expired key is named" "EXPIRED" "$(rsk ec2 pass)"

rm -f "$KEY.expires"; fresh
want "no recorded expiry is said" "no expiry recorded" "$(rsk ec2 pass)"

fresh; echo "$((now + 86400))" > "$KEY.expires"
rsk ec2 watch --interval 1 --ticks 1 --spill ec2 >/dev/null
rsk ec2 health >/dev/null; is "health passes after a poll" "0" "$?"
echo "ok 1 100 1" > "$TMP/run/watchd/round-spill-key.health"
rsk ec2 health >/dev/null; is "health fails once the poll is stale" "1" "$?"
