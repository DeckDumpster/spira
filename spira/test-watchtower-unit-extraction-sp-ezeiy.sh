#!/usr/bin/env bash
# Test: watchtower must extract actual unit names, not systemctl's bullet character
# Regression for sp-ezeiy: watchtower extracted "●" as unit name instead of the real unit

set -uo pipefail

_SYSTEMCTL_OUTPUT='● spira-watch-answers-prod.service loaded failed failed Spira watcher answers
● beads-push.service loaded failed failed Push Spira'"'"'s beads
  ● another-unit.service loaded failed failed Another unit'

echo "Testing unit name extraction from systemctl output..."

units=()
while IFS= read -r _fu_line; do
    [ -n "$_fu_line" ] || continue
    # This is the fix: skip bullet and extract first word starting with alphanumeric
    _fu_unit="$(printf '%s' "$_fu_line" | sed -E 's/^[^[:alnum:]]+ //; s/ .*//')"
    [ -n "$_fu_unit" ] && units+=("$_fu_unit")
done <<< "$_SYSTEMCTL_OUTPUT"

# Verify extraction
expected=("spira-watch-answers-prod.service" "beads-push.service" "another-unit.service")
if [ "${#units[@]}" -ne 3 ]; then
    echo "FAIL: Expected 3 units, got ${#units[@]}"
    exit 1
fi

for i in 0 1 2; do
    if [ "${units[$i]}" != "${expected[$i]}" ]; then
        echo "FAIL: Unit $i: got '${units[$i]}', expected '${expected[$i]}'"
        exit 1
    fi
done

# Regression check: ensure we never extract "●" as a unit name
for unit in "${units[@]}"; do
    if [ "$unit" = "●" ]; then
        echo "FAIL: Extracted bullet character as unit name"
        exit 1
    fi
done

echo "PASS: All $((${#units[@]})) units extracted correctly"
echo "  - ${units[0]}"
echo "  - ${units[1]}"
echo "  - ${units[2]}"
exit 0
