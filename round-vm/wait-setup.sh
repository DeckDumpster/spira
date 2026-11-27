#!/usr/bin/env bash
# Usage: wait-setup.sh <command-id> <instance-id>. Bound and poll interval (seconds) come from
# ROUND_AMI_SETUP_MAX and ROUND_AMI_SETUP_INTERVAL. Exit 0 setup succeeded, 1 it failed, 3 it
# passed the bound (hung). The last status is also written to ROUND_AMI_SETUP_STATUS_FILE if set.
set -uo pipefail
cid="${1:?usage: wait-setup.sh <command-id> <instance-id>}"
id="${2:?usage: wait-setup.sh <command-id> <instance-id>}"
max="${ROUND_AMI_SETUP_MAX:?}"
every="${ROUND_AMI_SETUP_INTERVAL:-30}"

invocation() {
    aws ssm get-command-invocation --command-id "$cid" --instance-id "$id" --query "$1" --output text
}
output_tail() {
    echo "--- setup $cid stdout (tail)"
    invocation StandardOutputContent | tail -n 100
    echo "--- setup $cid stderr (tail)"
    invocation StandardErrorContent | tail -n 100
}
finish() {
    [ -z "${ROUND_AMI_SETUP_STATUS_FILE:-}" ] || echo "$st" >"$ROUND_AMI_SETUP_STATUS_FILE"
    exit "$1"
}
hung() {
    echo "setup $cid on $id still not finished after ${elapsed}s (bound ${max}s); last status $st"
    output_tail
    echo "--- console output of $id (tail)"
    aws ec2 get-console-output --instance-id "$id" --latest --output text | tail -n 60
    finish 3
}

st=unknown
SECONDS=0
while :; do
    elapsed=$SECONDS
    # A failed call leaves the last status standing; its stderr goes to the log.
    if now="$(invocation Status)"; then
        st="$now"
        echo "setup $cid $st (${elapsed}s)"
        case "$st" in
            Success) finish 0 ;;
            Pending|InProgress|Delayed|Cancelling) ;;
            TimedOut)
                # SSM's delivery timeout is this same bound and starts slightly earlier, so it
                # can report first: within one interval of the bound it is the hang.
                [ "$((elapsed + every))" -lt "$max" ] || hung
                output_tail
                finish 1 ;;
            *) output_tail
               finish 1 ;;
        esac
    else
        echo "setup $cid poll-failed (${elapsed}s)"
    fi
    [ "$elapsed" -lt "$max" ] || hung
    sleep "$every"
done
