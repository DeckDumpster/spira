#!/usr/bin/env bash
#
# test-aeon-lease.sh — liveness lease: aeon_lease_minutes (lib.sh) renders the pane
#   countdown from the deadline file, and fayth_get proves the shipped chamber fayths
#   declare the lease minutes they actually mean.
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
# is deleted with them; aeon_lease_minutes and fayth_get below are unaffected and stay.
# UC-aeon-execution-08's statement (the lease renew/lapse/kill table) has no remaining bash
# suite and is marked [use_case.uncovered] in docs/test-plan/aeon-execution.toml (see §13
# there); UC-aeon-execution-09 (the thrash wall) stays covered by test-thrash.sh.
#
# No database, no network, under a second.
#
# defect: sp-9ix, sp-sv34w
# tier: T1
# covers: spira/lib.sh aeon/src/* cockpit-collect/src/* cockpit/ops/src/health.rs
# scar: the STALL_BEATS/model_idle apparatus was replaced by a liveness lease on trace growth; suites covering the old mechanism were testing code that no longer ran.
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

# Helper: run aeon_lease_minutes in a clean environment.
# Optional third arg: a pinned unix timestamp passed as SPIRA_NOW to fix the clock.
alm() {
    local bead="$1" run_dir="$2" now_arg="${3:-}"
    env -i PATH="$PATH" HOME="$TMP" LC_ALL=C.UTF-8 \
        SPIRA_CONF="$TMP/no.conf" SPIRA_RUN="$run_dir" \
        ${now_arg:+SPIRA_NOW="$now_arg"} \
        bash -c '. "$1"/lib.sh; aeon_lease_minutes "$2"' _ "$HERE" "$bead" 2>/dev/null
}

echo "aeon_lease_minutes — deadline file is the single source for the pane"

RUN="$TMP/run"
mkdir -p "$RUN/aeon"
BEAD="sp-test01"

# Case 1: no lease file → renders ?
result="$(alm "$BEAD" "$RUN")"
is "no lease file renders ?" "?" "$result"

# Case 2: a future deadline → positive countdown
# Pin now so deadline - now = 600 exactly, regardless of subshell timing.
pinned_now=$(date +%s)
future=$(( pinned_now + 600 ))
printf '%s' "$future" > "$RUN/aeon/$BEAD.lease"
result="$(alm "$BEAD" "$RUN" "$pinned_now")"
is "a 600s future deadline renders ~10m" "10" "$result"

# Case 3: a past deadline → negative (expired)
past=$(( $(date +%s) - 120 ))
printf '%s' "$past" > "$RUN/aeon/$BEAD.lease"
result="$(alm "$BEAD" "$RUN")"
# -2 (120 seconds past / 60 = 2 minutes expired)
is "a past deadline renders negative minutes" "-2" "$result"

# Case 4: an empty file → renders ?
: > "$RUN/aeon/$BEAD.lease"
result="$(alm "$BEAD" "$RUN")"
is "an empty lease file renders ?" "?" "$result"

# Case 5: a non-numeric file → renders ?
printf 'not-a-number' > "$RUN/aeon/$BEAD.lease"
result="$(alm "$BEAD" "$RUN")"
is "a non-numeric lease file renders ?" "?" "$result"

# Case 6: empty bead id → renders ?
result="$(alm "" "$RUN")"
is "an empty bead id renders ?" "?" "$result"

echo
echo "the shipped chamber fayths declare what the aeon actually enforces"
# defect: every fayth declared FAYTH_LEASE_MINUTES beside a FAYTH_LEASE_SECONDS=600 that
# silently overrode it — the builder's declared 90-minute lease was a 10-minute lease in
# effect. Pinned against the real chamber files, not a fixture copy of them.
fg() {
    env -i PATH="$PATH" HOME="$TMP" LC_ALL=C.UTF-8 SPIRA_HOME="$HERE" SPIRA_CONF="$TMP/no.conf" \
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
