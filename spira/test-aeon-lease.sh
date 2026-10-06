#!/usr/bin/env bash
#
# test-aeon-lease.sh — liveness lease: aeon_lease_minutes was lib.sh; RETIRED (wave 4.34,
#   sp-27d3d), ported to aeon::trace::aeon_lease_minutes, whose own unit test
#   (aeon_lease_minutes_table, aeon/src/trace.rs) now drives this suite's six cases (no
#   lease file, a future deadline, a past deadline, an empty file, a non-numeric file, an
#   empty bead id) directly against the function rather than through a bash sourcing shim.
#   fayth_get (below) is unrelated to that family and is unaffected.
#
#   ./test-aeon-lease.sh
#
# MERGED FROM test-thrash-wall.sh (sp-eq8a4.2.2): the lease and the thrash wall used to be
# two copies of aeon.sh's heartbeat subshell, one per suite, each re-deriving the same three
# `if`s the tick actually runs.
#
# RETIRED (sp-j89pd, wave 4.2): fayth_lease_seconds, hb_wait_outcome and hb_tick (lib.sh) had
# zero live callers — the renew/lapse/thrash decision is now aeon::decide::hb_tick
# (aeon/src/decide.rs), exercised by its own `hb_tick_table` unit test (whose comment names
# this file and test-thrash.sh as the bash rows it replaces). The trip decision's T1 table
# is deleted with them; fayth_get below is unaffected and stays.
# UC-aeon-execution-08's statement (the lease renew/lapse/kill table) has no remaining bash
# suite and is marked [use_case.uncovered] in docs/test-plan/aeon-execution.toml (see §13
# there); UC-aeon-execution-09 (the thrash wall) stays covered by test-thrash.sh.
#
# No database, no network, under a second.
#
# defect: sp-9ix, sp-sv34w
# tier: T1
# covers: aeon/src/trace.rs cockpit-collect/src/* cockpit/ops/src/health.rs
# scar: the STALL_BEATS/model_idle apparatus was replaced by a liveness lease on trace growth; suites covering the old mechanism were testing code that no longer ran.
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

echo
echo "the shipped chamber fayths declare what the aeon actually enforces"
# defect: every fayth declared FAYTH_LEASE_MINUTES beside a FAYTH_LEASE_SECONDS=600 that
# silently overrode it — the builder's declared 90-minute lease was a 10-minute lease in
# effect. Pinned against the real chamber files, not a fixture copy of them.
tl_config SPIRA_RUN="$TMP/run" SPIRA_CHAMBER="$HERE/chamber"
fg() {
    env -i PATH="$PATH" HOME="$TMP" LC_ALL=C.UTF-8 SPIRA_HOME="$HERE" SPIRA_CONF="$TMP/no.conf" \
        SPIRA_TOML="$SPIRA_TOML" \
        bash -c '. "$1"/lib.sh; fayth_get "$2" "$3" "$4"' _ "$HERE" "$1" "$2" "$3"
}
is "builder.fayth declares a 90-minute lease" "90" "$(fg builder FAYTH_LEASE_MINUTES 10)"
is "maechen.fayth declares a 70-minute lease" "70" "$(fg maechen FAYTH_LEASE_MINUTES 10)"
is "spike.fayth declares a 60-minute lease"   "60" "$(fg spike FAYTH_LEASE_MINUTES 10)"
for fy in builder czar groomer maechen ops spike; do
    if grep -q '^FAYTH_LEASE_SECONDS=' "$HERE/chamber/$fy.fayth"; then
        bad "$fy.fayth does not declare the dead FAYTH_LEASE_SECONDS key"
    else
        ok "$fy.fayth does not declare the dead FAYTH_LEASE_SECONDS key"
    fi
done

tl_summary
