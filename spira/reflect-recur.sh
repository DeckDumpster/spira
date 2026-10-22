#!/usr/bin/env bash
#
# reflect-recur.sh — delivers the judgement tier's CHECK: proposals once they recur.
#
#   reflect-recur.sh "<CHECK line>" [epoch]
#
# A proposal is keyed by its target (the text before the first '|'). One bead is filed when a
# key has SPIRA_REFLECT_RECUR_HITS rows inside SPIRA_REFLECT_RECUR_DAYS and none of them was
# filed, at most SPIRA_REFLECT_RECUR_WEEKLY_CAP a rolling week. A one-off files nothing.
set -uo pipefail
. "$(dirname "$0")/lib.sh"

line="${1:-}"
now="${2:-$(date +%s)}"
text="$(sed 's/^CHECK: *//' <<< "$line")"
[ -n "$text" ] || exit 0

key="$(printf '%s' "${text%%|*}" | tr 'A-Z' 'a-z' | tr -cs 'a-z0-9' '-' | sed 's/^-//; s/-$//' | cut -c1-80)"
[ -n "$key" ] || exit 0

mkdir -p "$(dirname "$SPIRA_REFLECT_LEDGER")"
(
    flock 9
    since=$((now - SPIRA_REFLECT_RECUR_DAYS * 86400))
    week=$((now - 7 * 86400))
    hits="$(awk -F'\t' -v k="$key" -v s="$since" '$2==k && $1>=s' "$SPIRA_REFLECT_LEDGER" 2>/dev/null | wc -l)"
    hits=$((hits + 1))
    filed_key="$(awk -F'\t' -v k="$key" -v s="$since" '$2==k && $1>=s && $3==1' "$SPIRA_REFLECT_LEDGER" 2>/dev/null | wc -l)"
    filed_week="$(awk -F'\t' -v s="$week" '$3==1 && $1>=s' "$SPIRA_REFLECT_LEDGER" 2>/dev/null | wc -l)"
    filed=0
    if [ "$hits" -ge "$SPIRA_REFLECT_RECUR_HITS" ] && [ "$filed_key" -eq 0 ] \
        && [ "$filed_week" -lt "$SPIRA_REFLECT_RECUR_WEEKLY_CAP" ]; then
        if "$(dirname "$0")/bead.sh" file "Sentinel check proposed ${hits}x by the judgement tier: ${text%%|*}" \
            --for builder --repo spira --body-file - >/dev/null <<BODY
## Build

Implement the deterministic check the judgement tier (reflect.sh) proposed ${hits} times in
${SPIRA_REFLECT_RECUR_DAYS} days, so it is never asked about this stall again:

${text}

Key: ${key}
BODY
        then
            filed=1
            log "reflect: recurring CHECK '$key' filed as a bead"
        fi
    fi
    printf '%s\t%s\t%s\t%s\n' "$now" "$key" "$filed" "$text" >> "$SPIRA_REFLECT_LEDGER"
) 9>"$SPIRA_REFLECT_LEDGER.lock"
