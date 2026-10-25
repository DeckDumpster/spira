#!/bin/sh
# gate-verdicts.sh <bead> <lifecycle state> <min gate runs> — exit 3 until the bead is in that state, 9 if
# it is in another state or its gate ran fewer times than asked, 0 when both hold.
set -u
bead="$1" want="$2" runs="$3"
state=$(env -u SPIRA_DB -u SPIRA_LC_SOCKET -u SPIRA_LC_PASSWORD_FILE sim probe "$SIM_WORLD" | tr -d ' ' | grep "\"bead\":\"$bead\"" | grep -o '"lc_state":"[A-Z_]*"')
[ -n "$state" ] || exit 3
[ "$state" = "\"lc_state\":\"$want\"" ] || exit 9
n=$(grep -cx "$bead" "$SIM_WORLD/config/gate-calls" 2>/dev/null || true)
[ "${n:-0}" -ge "$runs" ] || exit 9
