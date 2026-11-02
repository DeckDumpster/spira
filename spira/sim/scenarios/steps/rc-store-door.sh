#!/bin/sh
# rc-store-door.sh — take a lease through the round controller's store door, then read the lock
# back through spira-lc. Exit 0 when the probe exited 0 and the watch shows the put of
# locks/round/lease-probe/<id>; 9 otherwise, with both answers on stderr.
set -u
probe=$(round-controller lease probe --secs 0 --out "$SIM_WORLD/probe.out" 2>&1)
prc=$?
watch=$(spira-lc watch locks/round/lease-probe/ --from-rev 0 --wait-ms 0 2>&1)
fail() {
    printf 'rc-store-door: %s\nrc-store-door: probe (exit %s): %s\nrc-store-door: watch: %s\n' "$1" "$prc" "$probe" "$watch" >&2
    exit 9
}
[ "$prc" = 0 ] || fail "the lease probe failed"
printf '%s' "$watch" | python3 -c '
import json, re, sys
events = json.load(sys.stdin).get("events", [])
sys.exit(0 if any(re.fullmatch(r"locks/round/lease-probe/[0-9a-f]{16}", e["key"]) and e.get("value") is not None for e in events) else 1)
' 2>/dev/null || fail "the watch shows no put of a lease-probe lock"
exit 0
