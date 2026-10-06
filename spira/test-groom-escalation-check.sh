#!/usr/bin/env bash
#
# test-groom-escalation-check.sh — the groom escalation check's configuration surface:
# SPIRA_GROOM_ASK_LABEL is a conf key and defaults to groom-asked.
#
#   ./test-groom-escalation-check.sh
#
# WHAT THIS SUITE GUARDED
# -----------------------
# The groomer had a defect: it logged "ESCALATED sp-X" without ever calling mail, and the
# sentinel accepted the log line as evidence (CHECK5 only checks the line exists).
# FAYTH_GROOM_ESCALATION_CHECK=1 tells the aeon to verify the claim against the database at
# close: if no ask bead was created in this session naming that bead, the trigger is reopened
# and poisoned.
#
# RETIRED (wave 4.34, sp-27d3d): groom_claims_verified was lib.sh; it is ported to
# aeon::trace::groom_claims_verified, called in-process from Run::verdict
# (aeon/src/verdict.rs). Its T1 table (no escalation keyword, a claim with no ask bead, a
# fresh matching ask, an ask bead created before the session epoch, the id named in the
# description rather than the title, two claims with only one backed, "flagged" as a claim
# keyword, and an empty log) is groom_claims_verified_table (aeon/src/trace.rs).
#
# RETIRED (sp-v62vn): the T3 rows that drove the real aeon end to end. The check runs in the
# verdict's closed branch (`groom_escalation_check && closed`), and every session is
# restricted now and hands its bead on only through the work verbs, so
# decide::builder_closed is false for every session and the check is reached by none. Those
# rows (scrubber poisoned on an unbacked claim, not poisoned with a matching ask, a tiler
# without the key not held to it) asserted that unreachable path and are deleted, not
# rewritten; UC-aeon-execution-16 is marked uncovered in docs/test-plan/aeon-execution.toml.
#
# tier: T1
# covers: spira/conf.sh
# defect: sp-yr4ih
# scar: groomer wrote ESCALATED to the groom log without calling mail; sentinel accepted the log line as evidence of the escalation
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
# The tree's tools for a minimal-PATH run (sp-gypjk): where this suite's PATH finds the
# tree's build, and the tree's own spira/.
TOOLS="$(dirname "$(command -v spira-config)"):$HERE"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

echo "test-groom-escalation-check.sh"

echo
echo "SPIRA_GROOM_ASK_LABEL is in the conf key list and defaults to groom-asked:"
keys="$(env -i HOME="$TMP" PATH="$TOOLS:/usr/bin:/bin" SPIRA_CONF="$TMP/none.conf" SPIRA_TOML="$SPIRA_TOML" \
    bash -c '. "'"$HERE"'/conf.sh" && printf "%s" "$SPIRA_CONF_KEYS"' 2>/dev/null)"
want "SPIRA_GROOM_ASK_LABEL is in the key list" "SPIRA_GROOM_ASK_LABEL" "$keys"

val="$(env -i HOME="$TMP" PATH="$TOOLS:/usr/bin:/bin" SPIRA_CONF="$TMP/none.conf" SPIRA_TOML="$SPIRA_TOML" \
    bash -c '. "'"$HERE"'/conf.sh" && printf "%s" "$SPIRA_GROOM_ASK_LABEL"' 2>/dev/null)"
is "SPIRA_GROOM_ASK_LABEL defaults to groom-asked" "groom-asked" "$val"

tl_summary
