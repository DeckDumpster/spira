#!/bin/sh
# gate-verdicts.sh <bead> <lifecycle state> <next mode> — once the bead's gate has run, exit 9 unless the bead
# sits in that state, else steer its gate to <next mode>. Exit 3 until the gate has run; 0 once done.
set -u
bead="$1" want="$2" next="$3"
done="$SIM_WORLD/config/gate-checked-$bead"
[ -f "$done" ] && exit 0
n=$(grep -cx "$bead" "$SIM_WORLD/config/gate-calls" 2>/dev/null || true)
[ "${n:-0}" -ge 1 ] || exit 3
state=$(env -u SPIRA_DB -u SPIRA_LC_SOCKET -u SPIRA_LC_PASSWORD_FILE sim probe "$SIM_WORLD" | tr -d ' ' | grep "\"bead\":\"$bead\"" | grep -o '"lc_state":"[A-Z_]*"')
[ "$state" = "\"lc_state\":\"$want\"" ] || exit 3
echo "$bead $next" >> "$SIM_WORLD/config/gate-control"
: > "$done"
