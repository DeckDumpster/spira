#!/usr/bin/env bash
# Usage: wait-image.sh <ami-id>. Bound and poll interval (seconds) come from
# ROUND_AMI_WAIT_MAX and ROUND_AMI_WAIT_INTERVAL; the stock CLI waiter gives up at 10 minutes.
set -uo pipefail
ami="${1:?usage: wait-image.sh <ami-id>}"
max="${ROUND_AMI_WAIT_MAX:-3600}"
every="${ROUND_AMI_WAIT_INTERVAL:-30}"
waited=0
while :; do
    st="$(aws ec2 describe-images --image-ids "$ami" --query 'Images[0].State' --output text)" \
        || { echo "describe-images failed for $ami" >&2; exit 1; }
    case "$st" in
        available) exit 0 ;;
        pending) ;;
        *) echo "image $ami is $st" >&2; exit 1 ;;
    esac
    if [ "$waited" -ge "$max" ]; then
        echo "image $ami still $st after ${waited}s" >&2
        exit 1
    fi
    echo "image $ami $st (${waited}s)"
    sleep "$every"
    waited=$((waited + every))
done
