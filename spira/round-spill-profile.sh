#!/usr/bin/env bash
#
# round-spill-profile.sh — prepares the AWS profile and tailscale key location the EC2 spill
# provider reads, from the registry values. It never writes a credential or an auth key: the
# operator pastes the access key into the profile and the auth key into the key file once.
#
#   round-spill-profile.sh ensure    set the profile's region, create the key file's directory 0700
#   round-spill-profile.sh status    print what is still the operator's to supply; non-zero if any
#
# covers: spira/round-spill-profile.sh spira/conf.d/SPIRA_ROUND_VM_EC2_*
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/lib.sh"

AWS="${SPIRA_AWS:-aws}"

cmd_ensure() {
    local profile="$SPIRA_ROUND_VM_EC2_PROFILE" region="$SPIRA_ROUND_VM_EC2_REGION" key="$SPIRA_ROUND_VM_EC2_TAILSCALE_KEY_FILE"
    [ -n "$profile" ] && [ -n "$region" ] && [ -n "$key" ] || { echo "round-spill-profile: EC2 profile, region and key file must all be configured" >&2; return 1; }
    "$AWS" configure set region "$region" --profile "$profile" || return 1
    mkdir -p "$(dirname "$key")" && chmod 0700 "$(dirname "$key")"
}

cmd_status() {
    local profile="$SPIRA_ROUND_VM_EC2_PROFILE" key="$SPIRA_ROUND_VM_EC2_TAILSCALE_KEY_FILE" rc=0
    "$AWS" configure get aws_access_key_id --profile "$profile" >/dev/null 2>&1 \
        || { echo "profile $profile: no access key; paste it with: aws configure --profile $profile"; rc=1; }
    [ -s "$key" ] || { echo "tailscale key file $key: absent or empty; write the auth key there (0600) and its expiry (epoch seconds) to $key.expires"; rc=1; }
    return $rc
}

case "${1:-}" in
    ensure) cmd_ensure ;;
    status) cmd_status ;;
    *) echo "usage: round-spill-profile.sh ensure | status" >&2; exit 2 ;;
esac
