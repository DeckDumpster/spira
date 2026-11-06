#!/usr/bin/env bash
# test-units-optional.sh — units.sh gates optional units by what is present on the box.
#
# Merges test-install-loom-broker.sh and test-install-mail-deliver.sh
# (docs/test-plan/instance-lifecycle.md cluster 7): both asserted the identical
# UNITS/OPTIONAL/ENABLE shape — present -> unit in UNITS and ENABLE; absent -> unit in
# OPTIONAL, absent from UNITS/ENABLE, note on stderr — over a different prerequisite each
# time. Since sp-gypjk the compiled binaries are no longer a gate — every release carries
# loom and broker in bin/ — so A/C assert they are installed unconditionally. inotifywait
# gated spira-mail-deliver.service until mail delivery moved into the scheduler; no unit
# depends on it now, so nothing here varies it.
#
# tier: T1
# covers: install/src/manifest.rs install/src/bin/units_install.rs systemd/spira-loom.service UC-instance-lifecycle-25
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
tl_config SPIRA_TEST_CASE_DEADLINE=90

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT
mkdir -p "$TMP/run"

# _units — units-install --list-templates (sp-31dm0: systemd/units.sh is retired; the
# manifest is install/src/manifest.rs now) in a subprocess, reshaped into the
# UNITS:/OPTIONAL:/ENABLE: lines this suite's assertions read. env -i
# SPIRA_RELEASE="$(tl_release)" keeps this suite's own environment out of it.
_units() {
    # SPIRA_WATCHERS no longer derives from SPIRA_HOME (one source of config, per Ryan
    # 2026-10-05): point it at the real manifest beside this suite, or units-install
    # refuses outright ("no watcher manifest at /fixture/userhome/...").
    tl_config SPIRA_INSTANCE=prod SPIRA_DOLT_DATA="" SPIRA_RUN="$TMP/run" \
        SPIRA_WATCHERS="$HERE/watchers"
    env -i SPIRA_RELEASE="$(tl_release)" \
        PATH="$PATH" \
        HOME="$HOME" \
        SPIRA_TOML="$SPIRA_TOML" \
        SPIRA_HOME="$HERE" \
        SPIRA_REPO="$(cd "$HERE/.." && pwd -P)" \
        SPIRA_CONF=/nonexistent \
        units-install --list-templates 2>"$TMP/notes" \
        | awk '
            /^UNITS / { units = units " " $2 }
            /^OPTIONAL / { optional = optional " " $2 }
            /^ENABLE / { enable = enable " " $2 }
            END {
                print "UNITS:" units
                print "OPTIONAL:" optional
                print "ENABLE:" enable
            }'
}

# ==========================================================================
echo "A: loom is installed unconditionally (a release always carries it)"
# ==========================================================================
a_out="$(_units)"; a_rc=$?
is     "A: units.sh exits 0" "0" "$a_rc"
want   "A: UNITS includes spira-loom.service" "spira-loom.service" \
       "$(printf '%s\n' "$a_out" | grep '^UNITS:')"
want   "A: ENABLE includes spira-loom-prod.service" "spira-loom-prod.service" \
       "$(printf '%s\n' "$a_out" | grep '^ENABLE:')"
nowant "A: OPTIONAL does not include spira-loom" "spira-loom" \
       "$(printf '%s\n' "$a_out" | grep '^OPTIONAL:')"

# ==========================================================================
echo "C: the broker intent queue is retired — no broker unit ships, none is enabled"
# ==========================================================================
nowant "C: UNITS names no spira-broker unit" "spira-broker" \
       "$(printf '%s\n' "$a_out" | grep '^UNITS:')"
nowant "C: ENABLE names no spira-broker unit" "spira-broker" \
       "$(printf '%s\n' "$a_out" | grep '^ENABLE:')"
nowant "C: OPTIONAL names no spira-broker unit" "spira-broker" \
       "$(printf '%s\n' "$a_out" | grep '^OPTIONAL:')"
SHIPPED_BROKER_UNITS="$(ls "$HERE/../systemd" 2>/dev/null | grep '^spira-broker' || true)"
is "C: the release ships no spira-broker template" "" "$SHIPPED_BROKER_UNITS"
BROKER_ENABLE_KEY="$(ls "$HERE/conf.d" 2>/dev/null | grep -x 'SPIRA_BROKER_ENABLE' || true)"
is "C: the config registry has no SPIRA_BROKER_ENABLE key" "" "$BROKER_ENABLE_KEY"

tl_summary
