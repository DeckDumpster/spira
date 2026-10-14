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

fresh
want "ec2-always is watched like ec2" "expires in 4 days" "$(rsk ec2-always pass)"

fresh
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
rsk ec2 watch --interval 1 --ticks 1 >/dev/null
rsk ec2 health >/dev/null; is "health passes after a poll" "0" "$?"
echo "ok 1 100 1" > "$TMP/run/watchd/round-spill-key.health"
rsk ec2 health >/dev/null; is "health fails once the poll is stale" "1" "$?"

mkdir -p "$TMP/bin"
printf '#!/usr/bin/env bash\necho "$*" >> "%s/aws.log"\n[ "$1 $2" = "configure get" ] && [ -e "%s/has-access" ] && echo AKIAX\n[ "$1 $2" = "configure get" ] && [ ! -e "%s/has-access" ] && exit 1\nexit 0\n' "$TMP" "$TMP" "$TMP" > "$TMP/bin/aws"
chmod +x "$TMP/bin/aws"
KEY="$TMP/keydir/k.tskey"
rsp() {
    tl_config SPIRA_RUN="$TMP/run" SPIRA_ROUND_VM_EC2_PROFILE=prof-x SPIRA_ROUND_VM_EC2_REGION=eu-test-9 SPIRA_ROUND_VM_EC2_TAILSCALE_KEY_FILE="$KEY"
    env -i PATH="$PATH" HOME="$TMP" SPIRA_TOML="$SPIRA_TOML" SPIRA_AWS="$TMP/bin/aws" bash "$HERE/round-spill-profile.sh" "$@" 2>&1
}
rsp ensure >/dev/null; is "ensure succeeds" "0" "$?"
want "ensure sets the configured profile's region" "configure set region eu-test-9 --profile prof-x" "$(cat "$TMP/aws.log")"
is "ensure creates the key directory 0700" "700" "$(stat -c %a "$TMP/keydir")"
nowant "ensure never writes a credential" "aws_access_key_id --profile prof-x" "$(grep ' set ' "$TMP/aws.log")"
out="$(rsp status)"; rc=$?
is "status fails with nothing supplied" "1" "$rc"
want "status names the missing access key" "no access key" "$out"
want "status names the missing tailscale key" "absent or empty" "$out"
touch "$TMP/has-access"; echo tskey > "$KEY"
rsp status >/dev/null; is "status passes once both are supplied" "0" "$?"
