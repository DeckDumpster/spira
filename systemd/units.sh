#!/usr/bin/env bash
# units.sh — per-instance unit-naming library for the Spira harness.
#
# SOURCE THIS FILE; DO NOT EXECUTE IT. It is not a script with a main body —
# it is a library whose entire content is top-level initialisation. Executing
# it directly produces the same result as sourcing it into an empty shell, but
# sourcing is the intended contract: the functions and arrays it defines belong
# to the caller's shell.
#
# WHAT IT PROVIDES
# ----------------
#   inst_name <template-name>     — compute the per-instance installed unit name
#   inst_watch_name <watcher>     — compute the per-instance watcher unit name
#   UNITS                         — all unit template names (incl. optional conditionals)
#   ENABLE                        — unit names (per-instance) that should be enabled
#   OPTIONAL                      — template names excluded from UNITS by design
#   _watch_names                  — plain watcher names from the manifest (e.g. "testview")
#   watch_units                   — space-joined per-instance watcher unit names,
#                                   space-padded at both ends for membership tests
#
# WHAT CALLERS MUST SET BEFORE SOURCING
# --------------------------------------
#   SPIRA_INSTANCE     the instance suffix (e.g. "prod", "test")
#   SPIRA_HOME         the harness spira/ directory (provides watchd.sh)
#   SPIRA_DOLT_DATA    if non-empty, include and enable dolt-beads.service
#   SPIRA_TESTDB_DATA  if non-empty, include dolt-beads-test.service (not enabled)
#
# These are all set by conf.sh, so any caller that sources conf.sh first already
# has them. install.sh sources conf.sh before sourcing this file.
#
# GUARD AGAINST DOUBLE-SOURCING. A script that sources both this file and a
# helper that also sources it gets a second pass through the unit-list logic for
# free, which appends duplicate entries to UNITS and ENABLE. The guard prevents
# that: the second source call returns immediately.
[ -n "${_SPIRA_UNITS_LOADED:-}" ] && return 0
_SPIRA_UNITS_LOADED=1

# UNIT NAME MAPPING. Each spira-*.service/.timer gets a per-instance suffix appended before
# the extension so two instances can coexist on one machine without colliding unit names.
# Non-spira units (cockpit-ensure, concierge, beads-push, dolt-beads) are shared and keep
# their plain names. The watcher template (spira-watch@.service) is excluded here; its
# per-watcher per-instance names are built by inst_watch_name.
inst_name() {
    local u="$1"
    case "$u" in
        spira-watch@.service)  printf '%s' "$u" ;;   # never installed directly; handled below
        spira-*.service)       printf '%s-%s.service' "${u%.service}" "$SPIRA_INSTANCE" ;;
        spira-*.timer)         printf '%s-%s.timer'   "${u%.timer}"   "$SPIRA_INSTANCE" ;;
        *)                     printf '%s' "$u" ;;
    esac
}

# WATCHER UNIT NAME. The manifest row named <wname> installs as this unit for this instance.
# spira-watch@<name>.service (watchd.sh output) → spira-watch-<name>-<instance>.service
inst_watch_name() { printf 'spira-watch-%s-%s.service' "$1" "$SPIRA_INSTANCE"; }

UNITS=(spira-sentinel.service spira-sentinel.timer
       spira-ops.service spira-ops.timer
       spira-auron.service spira-auron.timer
       spira-watchtower.service spira-watchtower.timer
       spira-skew.service spira-skew.timer
       spira-archivist.service spira-archivist.timer
       spira-cockpit.service
       spira-loom.service
       spira-watch@.service
       spira-watch-notify.service spira-watch-notify.timer
       spira-watch-refresh.service spira-watch-refresh.timer
       cockpit-ensure.service cockpit-ensure.timer
       concierge.service concierge.timer
       beads-push.service beads-push.timer
       spira-archive.service spira-archive.timer
       spira-suites.service spira-suites.timer
       spira-groom.service spira-groom.timer
       spira-maechen.service spira-maechen.timer
       spira-moot-sweep.service spira-moot-sweep.timer
       spira-verify-asks.service spira-verify-asks.timer
       spira-gate-check.service spira-gate-check.timer
       spira-pr-notify.service spira-pr-notify.timer
       )
# Only these get enabled. The .service behind a .timer is started BY the timer; enabling it
# as well would also run it once at boot, outside the schedule.
# Template names mapped through inst_name so the enabled unit matches its installed name.
_ENABLE_TMPL=(cockpit-ensure.timer concierge.timer spira-watch-refresh.timer
              beads-push.timer spira-sentinel.timer spira-ops.timer spira-auron.timer
              spira-watchtower.timer spira-skew.timer
              spira-archive.timer spira-suites.timer
              spira-archivist.timer spira-watch-notify.timer
              spira-groom.timer
              spira-maechen.timer
              spira-moot-sweep.timer
              spira-verify-asks.timer
              spira-gate-check.timer
              spira-pr-notify.timer
              spira-cockpit.service spira-loom.service)
ENABLE=()
for _t in "${_ENABLE_TMPL[@]}"; do ENABLE+=("$(inst_name "$_t")"); done
unset _t _ENABLE_TMPL

# UNITS THIS BOX DELIBERATELY DECLINED. A conditional unit is absent from UNITS on purpose,
# so unlisted must be told the difference between "not installed here" and "nobody ever
# listed it" — otherwise the check that exists to catch a forgotten unit cries wolf on every
# box without a Dolt server, and a check that is always red is a check nobody reads.
OPTIONAL=()

# promote.sh carries commits from the development checkout to a separate production
# checkout. In single-checkout mode there is no second checkout and its source and
# destination are the same directory, so the unit cannot do anything but fail. Installing
# it anyway put an un-suspendable fatal in every doctor run on such a box, for a unit
# whose absence was correct — and a check that is always red is a check nobody reads.
#
# The predicate is spira_single_checkout from conf.sh, shared with doctor, so the manifest
# and the diagnosis cannot reach different conclusions about the same box.
if declare -F spira_single_checkout >/dev/null 2>&1 && spira_single_checkout; then
    OPTIONAL+=(spira-promote.service spira-promote.timer)
else
    UNITS+=(spira-promote.service spira-promote.timer)
    ENABLE+=("$(inst_name spira-promote.timer)")
fi

# dolt-beads.service supervises the Dolt server itself, which is only this harness's business
# when the operator says so. Empty SPIRA_DOLT_DATA means they run the server their own way,
# and installing a unit that would fight them is worse than not installing one.
if [ -n "${SPIRA_DOLT_DATA:-}" ]; then
    UNITS+=(dolt-beads.service); ENABLE+=(dolt-beads.service)
else
    OPTIONAL+=(dolt-beads.service)
    echo "note: SPIRA_DOLT_DATA is empty — not installing dolt-beads.service." >&2
    echo "      Start your Dolt server yourself, or set it in ${SPIRA_CONF_FILE:-spira.conf}." >&2
fi

# dolt-beads-test.service is a second Dolt server for test fixtures. It is installed so
# that `systemctl --user start dolt-beads-test.service` works, but NOT enabled: script
# activation from testdb.sh replaces always-on (WantedBy=default.target is intentionally
# absent from the unit file). A box with no suite running pays nothing.
if [ -n "${SPIRA_TESTDB_DATA:-}" ]; then
    UNITS+=(dolt-beads-test.service)
    # Not in ENABLE — installed but not enabled at login; testdb.sh starts on demand.
else
    OPTIONAL+=(dolt-beads-test.service)
fi

# ONE INSTANCE PER `daemon` ROW, AND THE MANIFEST DECIDES WHICH. `log` rows name a file
# something else already writes, so they get no unit; enabling one would double up whatever
# is already producing it.
#
# A MALFORMED MANIFEST STOPS THE INSTALL, and it stops it HERE — before a single unit is
# rendered — so a refusal leaves the box as it was rather than half installed. Enabling the
# rows that happened to parse would be worse than refusing: a watcher that was never started
# looks exactly like a watcher with nothing to say, and this is the last moment anybody is
# looking (law-absence-needs-a-positive-control).
#
# _watch_names collects the plain watcher names (e.g., "testview") so --render and --diff
# can produce per-instance watcher unit files. watch_units accumulates the installed unit
# names (e.g., "spira-watch-testview-prod.service") for the prune membership check below.
_watch_names=()
watch_units=" "
if watch_list="$("$SPIRA_HOME/watchd.sh" units)"; then
    for _wu in $watch_list; do
        _wname="${_wu#spira-watch@}"; _wname="${_wname%.service}"
        _inst_wu="$(inst_watch_name "$_wname")"
        ENABLE+=("$_inst_wu")
        watch_units="$watch_units$_inst_wu "
        _watch_names+=("$_wname")
    done
    unset _wu _wname _inst_wu
else
    echo "install: the watcher manifest is malformed — installing none of it" >&2
    # `return` rather than `exit` because this file is sourced: `exit` here would
    # terminate the caller's shell. The caller checks the source's exit status and
    # calls `exit 1` itself when this function returns non-zero.
    return 1
fi
