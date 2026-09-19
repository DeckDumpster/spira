#!/usr/bin/env bash
#
# test-install-mail-deliver.sh — install skips spira-mail-deliver when inotifywait absent.
#
# WHAT THIS SUITE PROVES
# ----------------------
# Without inotifywait, spira-mail-deliver.sh exits 1 immediately, and the unit crash-loops
# under Restart=always. The fix: units.sh omits the unit when inotifywait is absent, so
# install.sh never installs or enables it. (sp-va1ej)
#
# TWO PROPERTIES are verified, both required for the fix to hold:
#
#   A  POSITIVE CONTROL: with inotifywait on PATH, UNITS includes spira-mail-deliver.service
#      and ENABLE includes the instance-qualified name.
#
#   B  SKIP: with inotifywait absent (simulated via command() shadow), spira-mail-deliver.service
#      appears in OPTIONAL and is absent from UNITS.
#
# covers: systemd/units.sh systemd/install.sh systemd/spira-mail-deliver.service
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
REAL_REPO="$(cd "$HERE/.." && pwd -P)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "${2:-}"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }

echo "test-install-mail-deliver.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

# query_units <with|without> — source units.sh in a subprocess, print its UNITS and
# OPTIONAL arrays.  "with" uses the real command; "without" shadows the `command` builtin
# so that `command -v inotifywait` returns 1, simulating the binary being absent.
# conf.sh resets PATH to include /usr/bin regardless of the caller's PATH, so PATH
# manipulation cannot hide a system-installed inotifywait — the function shadow is the
# only reliable lever inside the sourcing context.
query_units() {
    local mode="$1"
    bash - <<SUBSH
set -uo pipefail
$([ "$mode" = "without" ] && cat <<'SHADOW'
# Shadow command so that `command -v inotifywait` returns 1.
command() {
    case "\$*" in
        "-v inotifywait"|"--version inotifywait") return 1 ;;
        *) builtin command "\$@" ;;
    esac
}
SHADOW
)
SPIRA_HOME="$HERE"
SPIRA_INSTANCE=prod
SPIRA_DOLT_DATA=
SPIRA_TESTDB_DATA=
SPIRA_CONF=/nonexistent
SPIRA_RUN="$TMP/run"
export SPIRA_HOME SPIRA_INSTANCE SPIRA_DOLT_DATA SPIRA_TESTDB_DATA SPIRA_CONF SPIRA_RUN
. "$HERE/../systemd/units.sh" 2>/dev/null
printf 'UNITS: %s\n' "\${UNITS[*]}"
printf 'OPTIONAL: %s\n' "\${OPTIONAL[*]}"
printf 'ENABLE: %s\n' "\${ENABLE[*]}"
SUBSH
}

mkdir -p "$TMP/run"

# ==========================================================================
echo
echo "A: POSITIVE CONTROL — inotifywait present → mail-deliver in UNITS and ENABLE:"
# ==========================================================================
pos_out="$(query_units with)"; pos_rc=$?
is   "A: units.sh exits 0 with inotifywait present"                 "0" "$pos_rc"
want "A: UNITS includes spira-mail-deliver.service"  "spira-mail-deliver.service" \
     "$(printf '%s\n' "$pos_out" | grep '^UNITS:')"
want "A: ENABLE includes spira-mail-deliver-prod.service"  "spira-mail-deliver-prod.service" \
     "$(printf '%s\n' "$pos_out" | grep '^ENABLE:')"
nowant "A: OPTIONAL does not include spira-mail-deliver"  "spira-mail-deliver" \
       "$(printf '%s\n' "$pos_out" | grep '^OPTIONAL:')"

# ==========================================================================
echo
echo "B: SKIP — inotifywait absent → mail-deliver in OPTIONAL, absent from UNITS:"
# ==========================================================================
neg_out="$(query_units without)"; neg_rc=$?
is   "B: units.sh exits 0 with inotifywait absent"                  "0" "$neg_rc"
nowant "B: UNITS does not include spira-mail-deliver.service" "spira-mail-deliver.service" \
       "$(printf '%s\n' "$neg_out" | grep '^UNITS:')"
nowant "B: ENABLE does not include spira-mail-deliver-prod.service" "spira-mail-deliver-prod.service" \
       "$(printf '%s\n' "$neg_out" | grep '^ENABLE:')"
want "B: OPTIONAL includes spira-mail-deliver.service" "spira-mail-deliver.service" \
     "$(printf '%s\n' "$neg_out" | grep '^OPTIONAL:')"

# ==========================================================================
echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" = 0 ]
