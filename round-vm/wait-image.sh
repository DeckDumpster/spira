#!/usr/bin/env bash
# Usage: wait-image.sh <ami-id>. Bound and poll interval (seconds) come from
# ROUND_AMI_WAIT_MAX and ROUND_AMI_WAIT_INTERVAL; the stock CLI waiter gives up at 10 minutes.
set -uo pipefail
ami="${1:?usage: wait-image.sh <ami-id>}"
max="${ROUND_AMI_WAIT_MAX:-3600}"
every="${ROUND_AMI_WAIT_INTERVAL:-30}"
waited=0
while :; do
    seen="$(aws ec2 describe-images --image-ids "$ami" \
        --query 'Images[0].[State,BlockDeviceMappings[0].Ebs.SnapshotId]' --output text)" \
        || { echo "describe-images failed for $ami" >&2; exit 1; }
    read -r st snap <<<"$seen"
    # The snapshot readings are report only: failing to read one never ends the wait.
    progress="" size="" unread=""
    if [[ "$snap" == snap-* ]]; then
        if reading="$(aws ec2 describe-snapshots --snapshot-ids "$snap" \
            --query 'Snapshots[0].[Progress,FullSnapshotSizeInBytes]' --output text)"; then
            read -r progress size <<<"$reading"
        else
            unread="size unread (describe-snapshots failed)"
        fi
    fi
    case "$st" in
        available)
            if [[ "$size" =~ ^[0-9]+$ ]]; then size="$size bytes"; else size="${unread:-size not reported}"; fi
            echo "image $ami available after ${SECONDS}s; snapshot ${snap:-unknown}: $size"
            exit 0 ;;
        pending) ;;
        *) echo "image $ami is $st" >&2; exit 1 ;;
    esac
    if [ "$waited" -ge "$max" ]; then
        echo "image $ami still $st after ${waited}s" >&2
        exit 1
    fi
    echo "image $ami $st (${waited}s) snapshot ${snap:-unknown} ${progress:-progress unknown}"
    sleep "$every"
    waited=$((waited + every))
done
