#!/bin/sh
# fences-in-vm.sh — exit 1 if a round ran its fences on the host (the world's gate saw a
# batcher-attr branch, or the batcher reported a host fence run); exit 3 until a round's
# fence-* results have arrived from the round VM; 0 once they have.
set -u
calls="$SIM_WORLD/config/gate-calls"
if grep -q '^batcher-attr/' "$calls" 2>/dev/null; then
    echo "fences-in-vm: the host gate ran a round's fences: $(grep '^batcher-attr/' "$calls" | head -3 | tr '\n' ' ')"
    exit 1
fi
if grep -qE 'fences refused, nothing judged|local round red \(fences\)' "$SIM_WORLD/exec.log" 2>/dev/null; then
    echo "fences-in-vm: the batcher ran fences on the host"
    exit 1
fi
found=$(find "$SPIRA_RUN/batch-results" -name 'fence-*.result' 2>/dev/null | head -3)
[ -n "$found" ] || exit 3
echo "fences-in-vm: fence results from the round VM: $(for f in $found; do printf '%s=%s ' "$(basename "$f" .result)" "$(cut -d' ' -f1 "$f")"; done)"
exit 0
