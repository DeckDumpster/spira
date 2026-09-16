#!/usr/bin/env bash
#
# test-priority-bands.sh — what the harness files about itself does not outrank what a user
# reported.
#
#   ./test-priority-bands.sh
#
# The loop takes ready work in priority order and runs one aeon at a time, so the two default
# bands decide what is ever reached. Suite reds are the harness reporting on itself; ingested
# issues are somebody outside saying something is broken for them. When the first band sat at
# or above the second, three days produced 103 self-maintenance commits against 8 on reported
# bugs, and 22 reported bugs were ready the whole time and never started.
#
# The RELATIONSHIP is what is asserted. Either default can move; what may not happen is the
# harness's own findings drawing an aeon ahead of a user's.
#
# covers: spira/conf.sh spira/suites.sh spira/gh-intake.sh spira/incident.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"

pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "${2:-}"; }

echo "test-priority-bands.sh"
echo

# THE SHIPPED DEFAULTS, read in an explicit minimal environment: a suite that inherits a real
# spira.conf is asserting about one box, not about what the repository ships.
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT
vals="$(env -i HOME="$TMP" PATH="$PATH" SPIRA_CONF="$TMP/none.conf" bash -c '
    . '"$HERE"'/conf.sh >/dev/null 2>&1
    printf "%s %s %s\n" "${SPIRA_SUITES_PRIORITY:-}" "${SPIRA_GH_INTAKE_PRIORITY:-}" "${SPIRA_INCIDENT_PRIORITY:-}"' 2>/dev/null)"
read -r self_pri user_pri inc_pri <<< "$vals"

# POSITIVE CONTROL. An unset value makes every comparison below vacuous, and an arithmetic
# test on an empty string is not a failure a reader would notice.
missing=""
case "$self_pri" in ''|*[!0-9]*) missing="$missing SPIRA_SUITES_PRIORITY" ;; esac
case "$user_pri" in ''|*[!0-9]*) missing="$missing SPIRA_GH_INTAKE_PRIORITY" ;; esac
case "$inc_pri"  in ''|*[!0-9]*) missing="$missing SPIRA_INCIDENT_PRIORITY" ;; esac
if [ -n "$missing" ]; then
    bad "both bands resolve to a number from a clean environment" "unreadable:$missing"
    printf '\n  %d passed, %d failed\n' "$pass" "$fail"; exit 1
fi
ok "every band resolves from a clean environment: suites=P$self_pri incidents=P$inc_pri intake=P$user_pri"

# LOWER NUMBER IS MORE URGENT, so a self-reported finding must sit at a HIGHER number.
if [ "$self_pri" -gt "$user_pri" ]; then
    ok "a suite red does not outrank a reported bug (P$self_pri behind P$user_pri)"
else
    bad "a suite red does not outrank a reported bug" \
        "suite reds file at P$self_pri and reported bugs at P$user_pri — self-maintenance draws the aeon first, indefinitely"
fi

# THE SAME FOR A MACHINE-OBSERVED INCIDENT. This defaulted to P1 inside incident.sh — the
# most urgent band — for every condition the harness noticed about itself. Callers that mean
# P1 say so on the call and are unaffected.
if [ "$inc_pri" -gt "$user_pri" ]; then
    ok "a machine-observed incident does not outrank a reported bug (P$inc_pri behind P$user_pri)"
else
    bad "a machine-observed incident does not outrank a reported bug" \
        "incidents open at P$inc_pri and reported bugs at P$user_pri"
fi

# BOTH SIDES READ THE KEY. A literal written into either program passes the comparison above
# while the configured value does nothing. Comments here and in those files state the rule
# being checked, so the match is against a comment-stripped view.
for pair in "suites.sh SPIRA_SUITES_PRIORITY" "gh-intake.sh SPIRA_GH_INTAKE_PRIORITY" "incident.sh SPIRA_INCIDENT_PRIORITY"; do
    f="${pair%% *}"; k="${pair##* }"
    if [ ! -r "$HERE/$f" ]; then bad "$f is readable" "not found"; continue; fi
    # Matched with a case, not a pipe into `grep -q`: that exits at the first match, the
    # upstream grep dies of SIGPIPE, and pipefail turns a successful search into 141
    # (law-no-grep-q-under-pipefail). It passed for the smaller file and failed for the
    # larger one, which is what that race looks like from outside.
    code="$(grep -vE '^[[:space:]]*#' "$HERE/$f")"
    if case "$code" in *"$k"*) true ;; *) false ;; esac; then
        ok "$f reads $k rather than a literal"
    else
        bad "$f reads $k rather than a literal" "the key appears nowhere in its code"
    fi
done

echo
printf '  %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
