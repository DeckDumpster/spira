#!/usr/bin/env bash
# test-units-optional.sh — units.sh gates optional units by what is present on the box.
#
# Merges test-install-loom-broker.sh and test-install-mail-deliver.sh
# (docs/test-plan/instance-lifecycle.md cluster 7): both asserted the identical
# UNITS/OPTIONAL/ENABLE shape — present -> unit in UNITS and ENABLE; absent -> unit in
# OPTIONAL, absent from UNITS/ENABLE, note on stderr — over a different prerequisite each
# time. Since sp-gypjk the compiled binaries are no longer a gate — every release carries
# loom and broker in bin/ — so A/C assert they are installed unconditionally, and only
# inotifywait (an external program) still gates a unit.
#
# tier: T1
# covers: systemd/units.sh systemd/install.sh systemd/spira-mail-deliver.service systemd/spira-loom.service systemd/spira-broker.service UC-instance-lifecycle-25
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT
mkdir -p "$TMP/run"

# _units <inotify-mode> — source units.sh in a subprocess with that gate set, print its
# UNITS/OPTIONAL/ENABLE arrays.
# inotify-mode "without" shadows the `command` builtin so `command -v inotifywait` fails:
# conf.sh rebuilds PATH itself (unconditionally adding /usr/bin), so hiding a system
# inotifywait by editing PATH does not work — the shadow is the only reliable lever.
# env -i prevents any of the three from leaking in from this suite's own environment.
_units() {
    local inotify_mode="$1"
    env -i \
        PATH="$PATH" \
        SPIRA_HOME="$HERE" \
        SPIRA_INSTANCE=prod \
        SPIRA_DOLT_DATA= \
        SPIRA_TESTDB_DATA= \
        SPIRA_CONF=/nonexistent \
        SPIRA_RUN="$TMP/run" \
        bash -s -- "$@" 2>"$TMP/notes" <<'SUBSH'
set -uo pipefail
if [ "$1" = without ]; then
    command() {
        case "$*" in
            "-v inotifywait"|"--version inotifywait") return 1 ;;
            *) builtin command "$@" ;;
        esac
    }
fi
. "$SPIRA_HOME/../systemd/units.sh"
printf 'UNITS: %s\n' "${UNITS[*]}"
printf 'OPTIONAL: %s\n' "${OPTIONAL[*]}"
printf 'ENABLE: %s\n' "${ENABLE[*]}"
SUBSH
}

DEF_INOTIFY=with

# POSITIVE CONTROL FOR THE SUITE ITSELF: without inotifywait on this box, case E's
# "present" assertions could never fire — skip rather than report a false pass.
command -v inotifywait >/dev/null 2>&1 || skip "inotifywait not on PATH — cannot verify the present case"

# ==========================================================================
echo "A: loom is installed unconditionally (a release always carries it)"
# ==========================================================================
a_out="$(_units "$DEF_INOTIFY")"; a_rc=$?
is     "A: units.sh exits 0" "0" "$a_rc"
want   "A: UNITS includes spira-loom.service" "spira-loom.service" \
       "$(printf '%s\n' "$a_out" | grep '^UNITS:')"
want   "A: ENABLE includes spira-loom-prod.service" "spira-loom-prod.service" \
       "$(printf '%s\n' "$a_out" | grep '^ENABLE:')"
nowant "A: OPTIONAL does not include spira-loom" "spira-loom" \
       "$(printf '%s\n' "$a_out" | grep '^OPTIONAL:')"

# ==========================================================================
echo "C: broker is installed unconditionally; its timer waits for a producer"
# ==========================================================================
want   "C: UNITS includes spira-broker.service" "spira-broker.service" \
       "$(printf '%s\n' "$a_out" | grep '^UNITS:')"
want   "C: UNITS includes spira-broker.timer" "spira-broker.timer" \
       "$(printf '%s\n' "$a_out" | grep '^UNITS:')"
# SPIRA_BROKER_ENABLE is unset here (env -i), so the timer stays off with no producer —
# test-broker-units.sh covers the opt-in case.
nowant "C: ENABLE does not include spira-broker-prod.timer without SPIRA_BROKER_ENABLE" \
       "spira-broker-prod.timer" "$(printf '%s\n' "$a_out" | grep '^ENABLE:')"
nowant "C: OPTIONAL does not include spira-broker" "spira-broker" \
       "$(printf '%s\n' "$a_out" | grep '^OPTIONAL:')"

# ==========================================================================
echo "E/F: inotifywait present/absent (spira-mail-deliver.service)"
# ==========================================================================
e_out="$(_units "$DEF_INOTIFY")"; e_rc=$?
is     "E: units.sh exits 0 with inotifywait present" "0" "$e_rc"
want   "E: UNITS includes spira-mail-deliver.service" "spira-mail-deliver.service" \
       "$(printf '%s\n' "$e_out" | grep '^UNITS:')"
want   "E: ENABLE includes spira-mail-deliver-prod.service" "spira-mail-deliver-prod.service" \
       "$(printf '%s\n' "$e_out" | grep '^ENABLE:')"
nowant "E: OPTIONAL does not include spira-mail-deliver" "spira-mail-deliver" \
       "$(printf '%s\n' "$e_out" | grep '^OPTIONAL:')"

f_out="$(_units without)"; f_rc=$?
is     "F: units.sh exits 0 with inotifywait absent" "0" "$f_rc"
nowant "F: UNITS does not include spira-mail-deliver.service" "spira-mail-deliver.service" \
       "$(printf '%s\n' "$f_out" | grep '^UNITS:')"
nowant "F: ENABLE does not include spira-mail-deliver-prod.service" "spira-mail-deliver-prod.service" \
       "$(printf '%s\n' "$f_out" | grep '^ENABLE:')"
want   "F: OPTIONAL includes spira-mail-deliver.service" "spira-mail-deliver.service" \
       "$(printf '%s\n' "$f_out" | grep '^OPTIONAL:')"

tl_summary
