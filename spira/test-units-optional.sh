#!/usr/bin/env bash
# test-units-optional.sh — units.sh gates optional units by what is present on the box.
#
# Merges test-install-loom-broker.sh and test-install-mail-deliver.sh
# (docs/test-plan/instance-lifecycle.md cluster 7): both asserted the identical
# UNITS/OPTIONAL/ENABLE shape — present -> unit in UNITS and ENABLE; absent -> unit in
# OPTIONAL, absent from UNITS/ENABLE, note on stderr — over a different prerequisite each
# time (a compiled binary, inotifywait). One shared query helper carries both gates; each
# case flips exactly one away from a working default.
#
# tier: T1
# covers: systemd/units.sh systemd/install.sh systemd/spira-mail-deliver.service systemd/spira-loom.service systemd/spira-broker.service UC-instance-lifecycle-25
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT
mkdir -p "$TMP/run"

# Stub binaries: executable files that satisfy [ -x ], standing in for compiled loom/broker.
STUB_LOOM="$TMP/loom"
STUB_BROKER="$TMP/broker"
printf '#!/bin/sh\nexec "$@"\n' > "$STUB_LOOM"   && chmod +x "$STUB_LOOM"
printf '#!/bin/sh\nexec "$@"\n' > "$STUB_BROKER" && chmod +x "$STUB_BROKER"

# _units <loom-bin> <broker-bin> <inotify-mode> — source units.sh in a subprocess with
# exactly these three gates set, print its UNITS/OPTIONAL/ENABLE arrays.
# inotify-mode "without" shadows the `command` builtin so `command -v inotifywait` fails:
# conf.sh rebuilds PATH itself (unconditionally adding /usr/bin), so hiding a system
# inotifywait by editing PATH does not work — the shadow is the only reliable lever.
# env -i prevents any of the three from leaking in from this suite's own environment.
_units() {
    local loom_bin="$1" broker_bin="$2" inotify_mode="$3"
    env -i \
        PATH="$PATH" \
        SPIRA_HOME="$HERE" \
        SPIRA_INSTANCE=prod \
        SPIRA_DOLT_DATA= \
        SPIRA_TESTDB_DATA= \
        SPIRA_CONF=/nonexistent \
        SPIRA_RUN="$TMP/run" \
        SPIRA_LOOM_BIN="$loom_bin" \
        SPIRA_BROKER_BIN="$broker_bin" \
        bash -s -- "$@" 2>"$TMP/notes" <<'SUBSH'
set -uo pipefail
if [ "$3" = without ]; then
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
# _notes — the stderr from the most recent _units call. Read from the file rather than
# a variable _units sets itself: callers invoke _units inside `x="$(_units ...)"`, a command
# substitution subshell, so any variable _units assigned would die with that subshell.
_notes() { cat "$TMP/notes"; }

# DEFAULTS: everything present. Each case below flips exactly one argument away from these.
DEF_LOOM="$STUB_LOOM"; DEF_BROKER="$STUB_BROKER"; DEF_INOTIFY=with

# POSITIVE CONTROL FOR THE SUITE ITSELF: without inotifywait on this box, case E's
# "present" assertions could never fire — skip rather than report a false pass.
command -v inotifywait >/dev/null 2>&1 || skip "inotifywait not on PATH — cannot verify the present case"

# ==========================================================================
echo "A/B: loom binary present/absent"
# ==========================================================================
a_out="$(_units "$DEF_LOOM" "$DEF_BROKER" "$DEF_INOTIFY")"; a_rc=$?
is     "A: units.sh exits 0 with loom binary present" "0" "$a_rc"
want   "A: UNITS includes spira-loom.service" "spira-loom.service" \
       "$(printf '%s\n' "$a_out" | grep '^UNITS:')"
want   "A: ENABLE includes spira-loom-prod.service" "spira-loom-prod.service" \
       "$(printf '%s\n' "$a_out" | grep '^ENABLE:')"
nowant "A: OPTIONAL does not include spira-loom" "spira-loom" \
       "$(printf '%s\n' "$a_out" | grep '^OPTIONAL:')"

b_out="$(_units "" "$DEF_BROKER" "$DEF_INOTIFY")"; b_rc=$?
b_notes="$(_notes)"
is     "B: units.sh exits 0 with loom binary absent" "0" "$b_rc"
nowant "B: UNITS does not include spira-loom.service" "spira-loom.service" \
       "$(printf '%s\n' "$b_out" | grep '^UNITS:')"
nowant "B: ENABLE does not include spira-loom-prod.service" "spira-loom-prod.service" \
       "$(printf '%s\n' "$b_out" | grep '^ENABLE:')"
want   "B: OPTIONAL includes spira-loom.service" "spira-loom.service" \
       "$(printf '%s\n' "$b_out" | grep '^OPTIONAL:')"
want   "B: note printed for absent loom binary" "not installing spira-loom.service" "$b_notes"

# ==========================================================================
echo "C/D: broker binary present/absent"
# ==========================================================================
want   "C: UNITS includes spira-broker.service" "spira-broker.service" \
       "$(printf '%s\n' "$a_out" | grep '^UNITS:')"
want   "C: UNITS includes spira-broker.timer" "spira-broker.timer" \
       "$(printf '%s\n' "$a_out" | grep '^UNITS:')"
want   "C: ENABLE includes spira-broker-prod.timer" "spira-broker-prod.timer" \
       "$(printf '%s\n' "$a_out" | grep '^ENABLE:')"
nowant "C: OPTIONAL does not include spira-broker" "spira-broker" \
       "$(printf '%s\n' "$a_out" | grep '^OPTIONAL:')"

d_out="$(_units "$DEF_LOOM" "" "$DEF_INOTIFY")"; d_rc=$?
d_notes="$(_notes)"
is     "D: units.sh exits 0 with broker binary absent" "0" "$d_rc"
nowant "D: UNITS does not include spira-broker.service" "spira-broker.service" \
       "$(printf '%s\n' "$d_out" | grep '^UNITS:')"
nowant "D: ENABLE does not include spira-broker-prod.timer" "spira-broker-prod.timer" \
       "$(printf '%s\n' "$d_out" | grep '^ENABLE:')"
want   "D: OPTIONAL includes spira-broker.service" "spira-broker.service" \
       "$(printf '%s\n' "$d_out" | grep '^OPTIONAL:')"
want   "D: note printed for absent broker binary" "not installing spira-broker.service" "$d_notes"

# ==========================================================================
echo "E/F: inotifywait present/absent (spira-mail-deliver.service)"
# ==========================================================================
e_out="$(_units "$DEF_LOOM" "$DEF_BROKER" "$DEF_INOTIFY")"; e_rc=$?
is     "E: units.sh exits 0 with inotifywait present" "0" "$e_rc"
want   "E: UNITS includes spira-mail-deliver.service" "spira-mail-deliver.service" \
       "$(printf '%s\n' "$e_out" | grep '^UNITS:')"
want   "E: ENABLE includes spira-mail-deliver-prod.service" "spira-mail-deliver-prod.service" \
       "$(printf '%s\n' "$e_out" | grep '^ENABLE:')"
nowant "E: OPTIONAL does not include spira-mail-deliver" "spira-mail-deliver" \
       "$(printf '%s\n' "$e_out" | grep '^OPTIONAL:')"

f_out="$(_units "$DEF_LOOM" "$DEF_BROKER" without)"; f_rc=$?
is     "F: units.sh exits 0 with inotifywait absent" "0" "$f_rc"
nowant "F: UNITS does not include spira-mail-deliver.service" "spira-mail-deliver.service" \
       "$(printf '%s\n' "$f_out" | grep '^UNITS:')"
nowant "F: ENABLE does not include spira-mail-deliver-prod.service" "spira-mail-deliver-prod.service" \
       "$(printf '%s\n' "$f_out" | grep '^ENABLE:')"
want   "F: OPTIONAL includes spira-mail-deliver.service" "spira-mail-deliver.service" \
       "$(printf '%s\n' "$f_out" | grep '^OPTIONAL:')"

tl_summary
