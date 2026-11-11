#!/usr/bin/env bash
#
# test-cockpit.sh — `cockpit-collect once` prints and never writes the snapshot.
#
#   ./test-cockpit.sh
#
# The scheduler's cockpit tier tasks are the snapshot's only writer; an operator or a dwarf
# running `once` from anywhere must not overwrite what the pane reads, whatever its
# environment claims.
#
# defect: sp-20d
# tier: T1
# covers: cockpit-collect/src/* UC-cockpit-observability-01 UC-cockpit-observability-02
# scar: cockpit.sh wrote the snapshot unconditionally; a dwarf running a vendored copy or a manual invocation overwrote the live snapshot, and the pane read `?` for every key the interloper lacked.
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"
tl_config SPIRA_TEST_CASE_DEADLINE=60

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
RUN="$TMP/run"; mkdir -p "$RUN"
BASE_PATH="$PATH"

# Constant across every env -i SPIRA_RELEASE="$(tl_release)" invocation below unless re-declared (the ratelim "without a
# trace" case re-points SPIRA_RUN at EMPTY_RUN further down).
tl_config SPIRA_RUN="$RUN" SPIRA_DB="$TMP/nodb" SPIRA_REPO_MAP="$TMP/no-map" \
    SPIRA_RUNES=builder SPIRA_COCKPIT="$TMP"

# A minimal environment: no INVOCATION_ID, no real config, no inherited state.
run_cockpit() {
    env -i SPIRA_RELEASE="$(tl_release)" PATH="$BASE_PATH" HOME="$TMP" LC_ALL=C.UTF-8 \
        SPIRA_CONF="$TMP/no.conf" SPIRA_HOME="$HERE" SPIRA_REPO="$TMP" \
        SPIRA_TOML="$SPIRA_TOML" \
        "$@" \
        cockpit-collect once 2>/dev/null
}

# ======================================================================================
echo "once with no claim:"

out="$(run_cockpit)"
if [ ! -f "$RUN/cockpit.env" ]; then
    ok "snapshot is NOT written"
else
    bad "snapshot is NOT written" "file exists at $RUN/cockpit.env"
fi
want "keys are printed to stdout" "SP_AT=" "$out"
want "window key is present"      "SP_WINDOW_HOURS=" "$out"
want "pass duration key present"  "SP_PASS_SECS="    "$out"
# SP_AT ORDERING: the freshness badge bounds the OLDEST reading in the snapshot only when
# SP_AT is stamped at probe() START — before any section data is emitted. Verify this by
# checking that SP_AT appears in the output before SP_WINDOW_HOURS (the first domain key).
at_pos="$(printf '%s\n' "$out" | grep -n '^SP_AT=' | head -1 | cut -d: -f1)"
wh_pos="$(printf '%s\n' "$out" | grep -n '^SP_WINDOW_HOURS=' | head -1 | cut -d: -f1)"
if [ -n "$at_pos" ] && [ -n "$wh_pos" ] && [ "$at_pos" -lt "$wh_pos" ]; then
    ok "SP_AT is emitted before section data (ordering invariant)"
else
    bad "SP_AT is emitted before section data (ordering invariant)" \
        "SP_AT at line ${at_pos:-?}, SP_WINDOW_HOURS at line ${wh_pos:-?}"
fi

# ======================================================================================
# A positive control for the absence check above: a run that claims to be a systemd
# invocation, and one that sets the retired override, must still print and not write.
echo
echo "once never writes, whatever the environment claims:"

rm -f "$RUN/cockpit.env"
claim_out="$(run_cockpit env INVOCATION_ID=inv-42)"
want "the claiming run printed its keys" "SP_AT=" "$claim_out"
if [ ! -f "$RUN/cockpit.env" ]; then
    ok "INVOCATION_ID does not make once write"
else
    bad "INVOCATION_ID does not make once write" "file exists at $RUN/cockpit.env"
fi
nowant "once does not stamp a writer" "SP_WRITER=" "$claim_out"

# ======================================================================================
# RATE LIMIT WINDOWS — the ratelim seam.
#
# THE POSITIVE CONTROL IS FIRST. A check that only tests absence is indistinguishable from
# one pointed at the wrong thing; proving it fires on a known input first means a silent
# failure in the absence case is "found nothing" rather than "looked nowhere"
# (law-absence-needs-a-positive-control).
echo
echo "ratelim: with a prepared trace:"

# A minimal stream-json trace holding one rate_limit_event with unifiedWindows.
# utilization 0.27 for five_hour, 0.21 for seven_day. resetsAt is far in the future
# so the minutes-to-reset will be a large positive integer rather than 0.
cat > "$RUN/sp-ratelim-test.log" <<'TRACE'
{"type":"rate_limit_event","rate_limit_info":{"status":"allowed","resetsAt":9999999999,"unifiedWindows":{"five_hour":{"utilization":0.27,"resetsAt":9999999999},"seven_day":{"utilization":0.21,"resetsAt":9999999999}}}}
TRACE

rl_out="$(env -i SPIRA_RELEASE="$(tl_release)" PATH="$BASE_PATH" HOME="$TMP" LC_ALL=C.UTF-8 \
    SPIRA_CONF="$TMP/no.conf" SPIRA_HOME="$HERE" SPIRA_REPO="$TMP" \
    SPIRA_TOML="$SPIRA_TOML" \
    cockpit-collect probe ratelim 2>/dev/null)"

want  "five_hour utilisation present"        "SP_RATELIM_5H="     "$rl_out"
want  "seven_day utilisation present"        "SP_RATELIM_7D="     "$rl_out"
want  "five_hour percentage present"         "SP_RATELIM_5H_PCT=" "$rl_out"
want  "five_hour percentage is 27"           "SP_RATELIM_5H_PCT=27" "$rl_out"
want  "seven_day percentage is 21"           "SP_RATELIM_7D_PCT=21" "$rl_out"
want  "minutes to 5h reset is a number"      "SP_RATELIM_5H_MIN=" "$rl_out"
want  "minutes to 7d reset is a number"      "SP_RATELIM_7D_MIN=" "$rl_out"
want  "age key present"                      "SP_RATELIM_AGE="    "$rl_out"
nowant "no '?' for 5h pct when trace exists" "SP_RATELIM_5H_PCT=?" "$rl_out"
nowant "no '?' for 7d pct when trace exists" "SP_RATELIM_7D_PCT=?" "$rl_out"

# ======================================================================================
echo
echo "ratelim: without a trace (empty run dir):"

EMPTY_RUN="$TMP/empty-run"; mkdir -p "$EMPTY_RUN"
tl_config SPIRA_RUN="$EMPTY_RUN"
rl_empty="$(env -i SPIRA_RELEASE="$(tl_release)" PATH="$BASE_PATH" HOME="$TMP" LC_ALL=C.UTF-8 \
    SPIRA_CONF="$TMP/no.conf" SPIRA_HOME="$HERE" SPIRA_REPO="$TMP" \
    SPIRA_TOML="$SPIRA_TOML" \
    cockpit-collect probe ratelim 2>/dev/null)"

want "SP_RATELIM_5H is '?'" "SP_RATELIM_5H=?" "$rl_empty"
want "SP_RATELIM_7D is '?'" "SP_RATELIM_7D=?" "$rl_empty"
want "SP_RATELIM_5H_PCT is '?'" "SP_RATELIM_5H_PCT=?" "$rl_empty"

# ======================================================================================
echo
tl_summary
