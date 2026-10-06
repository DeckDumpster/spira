#!/usr/bin/env bash
#
# test-hold-liveness.sh — hold_alive, holder_alive and spira_holder_witnesses's hold
#   branch: pure pidfile logic against a scratch SPIRA_RUN. No database.
#
#   ./test-hold-liveness.sh
#
# THE PROPERTY UNDER TEST. A non-aeon session (brain, concierge, a hand-run tool) must be
# able to hold a bead for the duration of a piece of hand-work, and holder_alive must say
# so — otherwise strand.sh reclaims it mid-landing and charges an attempt that is not a
# fact about the work (sp-oz0b, sp-7pi). None of that needs a bead database: it is a
# question about whether a recorded pid is alive, answerable from /proc alone.
# test-hold.sh keeps the T2 integration — hold.sh and unhold.sh actually claiming and
# releasing a real bead.
#
# defect: sp-oz0b
# tier: T1
# covers: spira/lib.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
SPIRA_RUN="$TMP/run"; mkdir -p "$SPIRA_RUN"
tl_config SPIRA_RUN="$SPIRA_RUN"
export SPIRA_CONF="$TMP/no-such-conf"
SPIRA_REPO_MAP="$TMP/repo-map"
tl_config SPIRA_REPO_MAP="$SPIRA_REPO_MAP"
printf '# fixture — empty\n' > "$SPIRA_REPO_MAP"

# shellcheck disable=SC1090
. "$HERE/lib.sh"

echo "test-hold-liveness.sh"

# ======================================================================================
echo
echo "1. POSITIVE CONTROL: hold_alive recognises a live pid. Without this, every"
echo "   absence check below is meaningless."
# ======================================================================================
echo $$ > "$SPIRA_RUN/hold-sp-pos.pid"
if hold_alive "$SPIRA_RUN/hold-sp-pos.pid"; then ok "hold_alive sees a live pid"
else bad "hold_alive sees a live pid" "returned 1"; fi
rm -f "$SPIRA_RUN/hold-sp-pos.pid"

# ======================================================================================
echo
echo "2. NEGATIVE CONTROLS: hold_alive rejects a dead pid and a missing pidfile."
# ======================================================================================
echo 999999999 > "$SPIRA_RUN/hold-sp-neg.pid"
if hold_alive "$SPIRA_RUN/hold-sp-neg.pid"; then bad "hold_alive rejects a dead pid" "returned 0"
else ok "hold_alive rejects a dead pid"; fi
rm -f "$SPIRA_RUN/hold-sp-neg.pid"

if hold_alive "$SPIRA_RUN/hold-sp-none.pid"; then bad "hold_alive rejects a missing pidfile" "returned 0"
else ok "hold_alive rejects a missing pidfile"; fi

# ======================================================================================
echo
echo "3. holder_alive checks hold pidfiles. Before sp-oz0b, holder_alive only looked"
echo "   at aeon-*-<id>.pid."
# ======================================================================================
echo $$ > "$SPIRA_RUN/hold-sp-ha.pid"
if holder_alive sp-ha; then ok "holder_alive sees a hold pidfile"
else bad "holder_alive sees a hold pidfile" "returned 1"; fi
rm -f "$SPIRA_RUN/hold-sp-ha.pid"

# No pidfile at all: unheld.
if holder_alive sp-ha-none; then bad "holder_alive returns 1 with no pidfile" "returned 0"
else ok "holder_alive returns 1 with no pidfile"; fi

# ======================================================================================
echo
echo "4. spira_holder_witnesses: a hold pidfile alone is sufficient — it never reaches"
echo "   the database witness (holder_alive short-circuits first)."
# ======================================================================================
echo $$ > "$SPIRA_RUN/hold-sp-wit.pid"
witness="$(spira_holder_witnesses sp-wit)"; wrc=$?
is   "witnesses say held (exit 0)"        0                     "$wrc"
want "witnesses name a live process"      "a live process holds it" "$witness"
rm -f "$SPIRA_RUN/hold-sp-wit.pid"

echo
tl_summary
