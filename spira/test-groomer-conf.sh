#!/usr/bin/env bash
#
# test-groomer-conf.sh — groomer's conf keys and fayth declarations, static (UC-32).
#
#   ./test-groomer-conf.sh
#
# Split out of test-groomer.sh (a T1 behavioural suite) because these checks exercise no
# code path — they read conf.sh and chamber/groomer.fayth and assert on the values, which
# is a T0 lint concern, not a unit test.
#
# tier: T0
# covers: spira/conf.sh spira/chamber/groomer.fayth
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"
# The tree's tools for a minimal-PATH run (sp-gypjk): where this suite's PATH finds the
# tree's build, and the tree's own spira/.
TOOLS="$(dirname "$(command -v spira-config)"):$HERE"

T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT INT TERM
NONE="$T/none.conf"

echo "test-groomer-conf.sh"

# ==========================================================================================
echo
echo "SPIRA_GROOMER_LABEL is in the conf key list and defaults to 'groom'"
# ==========================================================================================
keys="$(env -i HOME="$T" PATH="$TOOLS:/usr/bin:/bin" SPIRA_CONF="$NONE" \
    bash -c '. "'"$HERE"'/conf.sh" && printf "%s" "$SPIRA_CONF_KEYS"' 2>/dev/null)"
want "SPIRA_GROOMER_LABEL is in the key list" "SPIRA_GROOMER_LABEL" "$keys"

val="$(env -i HOME="$T" PATH="$TOOLS:/usr/bin:/bin" SPIRA_CONF="$NONE" \
    bash -c '. "'"$HERE"'/conf.sh" && printf "%s" "$SPIRA_GROOMER_LABEL"' 2>/dev/null)"
is   "SPIRA_GROOMER_LABEL defaults to groom" "groom" "$val"

# ==========================================================================================
echo
echo "groomer lane is in SPIRA_LANES default"
# ==========================================================================================
lanes="$(env -i HOME="$T" PATH="$TOOLS:/usr/bin:/bin" SPIRA_CONF="$NONE" \
    bash -c '. "'"$HERE"'/conf.sh" && printf "%s" "$SPIRA_LANES"' 2>/dev/null)"
want "groomer lane is in SPIRA_LANES default" "groomer" "$lanes"

# ==========================================================================================
echo
echo "groomer.fayth declares FAYTH_LANE=groomer"
# ==========================================================================================
# Groomer is a party persona, not a task fayth. It must declare a lane so it draws from its
# own capacity rather than from SPIRA_MAX_AEONS.
if [ -f "$HERE/chamber/groomer.fayth" ]; then
    lane_val="$(env -i HOME="$T" PATH="$TOOLS:/usr/bin:/bin" SPIRA_CONF="$NONE" SPIRA_DB="$T/db" \
        bash -c '. "'"$HERE"'/conf.sh" && . "'"$HERE"'/chamber/groomer.fayth" && printf "%s" "${FAYTH_LANE:-}"' 2>/dev/null)"
    is   "groomer.fayth FAYTH_LANE=groomer" "groomer" "$lane_val"
else
    bad  "groomer.fayth" "not found at $HERE/chamber/groomer.fayth"
fi

# ==========================================================================================
echo
echo "groomer.fayth declares FAYTH_GROOM_ESCALATION_CHECK=1"
# ==========================================================================================
# The escalation check in aeon.sh is activated by this key. Without it, a groom pass
# that claims ESCALATED without filing an ask bead is never caught.
if [ -f "$HERE/chamber/groomer.fayth" ]; then
    gesc_val="$(env -i HOME="$T" PATH="$TOOLS:/usr/bin:/bin" SPIRA_CONF="$NONE" SPIRA_DB="$T/db" \
        bash -c '. "'"$HERE"'/conf.sh" && . "'"$HERE"'/chamber/groomer.fayth" && printf "%s" "${FAYTH_GROOM_ESCALATION_CHECK:-}"' 2>/dev/null)"
    is   "groomer.fayth FAYTH_GROOM_ESCALATION_CHECK=1" "1" "$gesc_val"
else
    bad  "groomer.fayth" "not found at $HERE/chamber/groomer.fayth"
fi

echo
tl_summary
