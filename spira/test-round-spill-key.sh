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
mkdir -p "$TMP/ok"; printf '#!/usr/bin/env bash\nexit 0\n' > "$TMP/ok/round-vm"; chmod +x "$TMP/ok/round-vm"
INBOX="$TMP/inbox.log"; KEY="$TMP/k.tskey"
: > "$KEY"

rsk() {
    local spill="$1"; shift
    tl_config SPIRA_RUN="$TMP/run" SPIRA_CONCIERGE_INBOX="$INBOX" SPIRA_ROUND_VM_SPILL="$spill" \
        SPIRA_ROUND_VM_EC2_TAILSCALE_KEY_FILE="$KEY" SPIRA_ROUND_VM_EC2_KEY_WARN_DAYS=9 SPIRA_ROUND_VM_EC2_PROBE_DEADLINE=7
    env -i PATH="$TMP/ok:$PATH" HOME="$TMP" SPIRA_TOML="$SPIRA_TOML" bash "$HERE/round-spill-key.sh" "$@" 2>&1
}
fresh() { rm -f "$TMP/run/watchd/round-spill-key.said" "$INBOX"; }

now="$(date +%s)"
echo "$((now + 5 * 86400 - 3600))" > "$KEY.expires"
fresh
is "off while spill is unset" "" "$(rsk '' pass)"
nowant "a planted near expiry is not reported while spill is off" "ROUND SPILL KEY" "$(cat "$INBOX" 2>/dev/null)"

fresh
want "ec2-always is watched like ec2" "expires in 4 days" "$(rsk ec2-always pass)"

fresh
want "ec2-only is watched like ec2" "expires in 4 days" "$(rsk ec2-only pass)"

mkdir -p "$TMP/stub"
printf '#!/usr/bin/env bash\n[ "$1" = template-check ] && exit "${TC_RC:-0}"\necho "$*" >> "%s/probe.log"\nif [ -e "%s/probe-red" ]; then echo "FAILED at dry-run launch: aws ec2: UnauthorizedOperation" >&2; exit 1; fi\n' "$TMP" "$TMP" > "$TMP/stub/round-vm"
chmod +x "$TMP/stub/round-vm"
rskp() {
    tl_config SPIRA_RUN="$TMP/run" SPIRA_CONCIERGE_INBOX="$INBOX" SPIRA_ROUND_VM_SPILL=ec2-only \
        SPIRA_ROUND_VM_EC2_TAILSCALE_KEY_FILE="$KEY" SPIRA_ROUND_VM_EC2_KEY_WARN_DAYS=9 \
        SPIRA_ROUND_VM_EC2_PROBE_DEADLINE="${DL:-7}"
    env -i PATH="$TMP/stub:$PATH" HOME="$TMP" SPIRA_TOML="$SPIRA_TOML" bash "$HERE/round-spill-key.sh" "$@" 2>&1
}
echo "$((now + 30 * 86400))" > "$KEY.expires"
fresh; rm -f "$TMP/probe.log"
is "a passing probe is quiet" "" "$(rskp pass)"
want "the watcher probes with --dry-run" "ec2-probe --dry-run" "$(cat "$TMP/probe.log")"
touch "$TMP/probe-red"
want "a failing probe alarms with the AWS error" "UnauthorizedOperation" "$(rskp pass)"
want "the alarm reaches the inbox" "ROUND SPILL LAUNCH" "$(cat "$INBOX")"
is "the same failure is said once" "" "$(rskp pass)"
rm -f "$TMP/probe-red"; rskp pass >/dev/null; touch "$TMP/probe-red"
want "a recurrence after recovery alarms again" "UnauthorizedOperation" "$(rskp pass)"
rm -f "$TMP/probe-red"
fresh
printf '#!/usr/bin/env bash\n[ "$1" = template-check ] && exit "${TC_RC:-0}"\n[ "$1" = status ] && exit 1\nsleep 6\nexit 0\n' > "$TMP/stub/round-vm"
is "a probe slower than the old 5s cap but inside the 7s deadline is quiet" "" "$(rskp pass)"
nowant "a slow successful probe leaves no alarm" "ROUND SPILL LAUNCH" "$(cat "$INBOX" 2>/dev/null)"
printf '#!/usr/bin/env bash\n[ "$1" = template-check ] && exit "${TC_RC:-0}"\n[ "$1" = status ] && exit 1\nsleep 5\nexit 0\n' > "$TMP/stub/round-vm"
rm -f "$TMP/run/watchd/round-spill-key.latency"
DL=1 out="$(rskp pass)"
want "a probe past the deadline says it could not decide" "probe could not decide" "$out"
nowant "a timeout is not reported as a refused launch" "cannot launch" "$(cat "$INBOX")"
nowant "a timeout is not reported as a launch failure" "not confirmed" "$(cat "$INBOX")"

now2="$(date +%s)"
status_stub() {
    printf '#!/usr/bin/env bash\n[ "$1" = template-check ] && exit "${TC_RC:-0}"\nif [ "$1" = status ]; then cat "%s/status.json"; exit 0; fi\nsleep 5\nexit 0\n' "$TMP" > "$TMP/stub/round-vm"
}
status_stub
cat > "$TMP/status.json" <<JSON
{"events":[{"vm":"ec2-aa","to":"provisioning","reason":"spilled to ec2: x","at":$((now2 - 300))},{"vm":"ec2-aa","to":"leased","reason":"spilled to ec2: x (provider: ec2)","at":$((now2 - 290))}]}
JSON
fresh
DL=1 out="$(rskp pass)"
is "a real launch inside the window leaves a slow dry-run unrun and the watcher quiet" "" "$out"
cat > "$TMP/status.json" <<JSON
{"events":[{"vm":"ec2-aa","to":"leased","reason":"spilled to ec2: x (provider: ec2)","at":$((now2 - 90000))}]}
JSON
fresh
DL=1 out="$(rskp pass)"
want "a real launch outside the window falls back to the dry-run" "probe could not decide" "$out"
cat > "$TMP/status.json" <<JSON
{"events":[{"vm":"ec2-bb","to":"provisioning","reason":"spilled to ec2: x","at":$((now2 - 120))},{"vm":"ec2-bb","to":"destroyed","reason":"aws ec2: An error occurred (VcpuLimitExceeded) when calling RunInstances","at":$((now2 - 100))}]}
JSON
fresh
want "a refused real launch alarms with its error" "VcpuLimitExceeded" "$(rskp pass)"
cat > "$TMP/status.json" <<JSON
{"events":[{"vm":"ec2-bb","to":"destroyed","reason":"aws ec2: VcpuLimitExceeded","at":$((now2 - 100))},{"vm":"ec2-cc","to":"leased","reason":"spilled to ec2: x (provider: ec2)","at":$((now2 - 50))}]}
JSON
fresh
is "a later successful launch supersedes an earlier refusal" "" "$(rskp pass)"
mkdir -p "$TMP/state"
printf '#!/usr/bin/env bash\n[ "$1" = template-check ] && exit "${TC_RC:-0}"\nsleep 30\n' > "$TMP/stub/round-vm"
rskd() {
    tl_config SPIRA_RUN="$TMP/run" SPIRA_CONCIERGE_INBOX="$INBOX" SPIRA_ROUND_VM_SPILL=ec2-only SPIRA_ROUND_VM_STATE_DIR="$TMP/state" \
        SPIRA_ROUND_VM_EC2_TAILSCALE_KEY_FILE="$KEY" SPIRA_ROUND_VM_EC2_KEY_WARN_DAYS=9 SPIRA_ROUND_VM_EC2_PROBE_DEADLINE=1
    env -i PATH="$TMP/stub:$PATH" HOME="$TMP" SPIRA_TOML="$SPIRA_TOML" bash "$HERE/round-spill-key.sh" "$@" 2>&1
}
echo "$((now + 30 * 86400))" > "$KEY.expires"
cat > "$TMP/state/pool.json" <<JSON
{"events":[{"vm":"ec2-dd","to":"provisioning","reason":"spilled to ec2: x","at":$((now2 - 700))},{"vm":"ec2-dd","to":"leased","reason":"spilled to ec2: x (provider: ec2)","at":$((now2 - 600))}]}
JSON
fresh
is "a hung round-vm and a timing-out probe stay quiet when pool.json holds a recent lease" "" "$(rskd pass)"
cat > "$TMP/state/pool.json" <<JSON
{"events":[]}
JSON
fresh
out="$(rskd pass)"
want "no launch in pool.json and a timing-out probe alarms" "probe could not decide" "$out"
want "the alarm names the window" "last 3600s" "$out"
rm -f "$TMP/probe.log"
printf '#!/usr/bin/env bash\n[ "$1" = template-check ] && exit "${TC_RC:-0}"\necho "FAILED at dry-run launch: aws ec2: UnauthorizedOperation" >&2; exit 1\n' > "$TMP/stub/round-vm"
fresh
want "a refused launch still quotes the AWS error" "cannot launch: FAILED at dry-run launch: aws ec2: UnauthorizedOperation" "$(rskp pass)"
echo "$((now + 5 * 86400 - 3600))" > "$KEY.expires"

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

printf '#!/usr/bin/env bash\n[ "$1" = template-check ] && { echo "local round template 9000 is missing on node pve: refusing the lease" >&2; exit 1; }\nexit 0\n' > "$TMP/stub/round-vm"
fresh
out="$(TC_RC=1 rskp pass)"
nowant "ec2-only never checks the local template" "ROUND TEMPLATE" "$out"
tl_config SPIRA_RUN="$TMP/run" SPIRA_CONCIERGE_INBOX="$INBOX" SPIRA_ROUND_VM_SPILL=ec2 \
    SPIRA_ROUND_VM_EC2_TAILSCALE_KEY_FILE="$KEY" SPIRA_ROUND_VM_EC2_KEY_WARN_DAYS=9
tpl() { env -i PATH="$TMP/stub:$PATH" HOME="$TMP" SPIRA_TOML="$SPIRA_TOML" bash "$HERE/round-spill-key.sh" pass 2>&1; }
fresh
want "a missing local template raises the watch row" "ROUND TEMPLATE: the local round pool cannot lease: local round template 9000 is missing" "$(tpl)"
want "the row reaches the inbox" "ROUND TEMPLATE" "$(cat "$INBOX")"
is "the same missing template is said once" "" "$(tpl)"
printf '#!/usr/bin/env bash\nexit 0\n' > "$TMP/stub/round-vm"
tpl >/dev/null
printf '#!/usr/bin/env bash\n[ "$1" = template-check ] && { echo "local round template 9000 is missing on node pve: refusing the lease" >&2; exit 1; }\nexit 0\n' > "$TMP/stub/round-vm"
want "a recurrence after recovery is said again" "ROUND TEMPLATE" "$(tpl)"
