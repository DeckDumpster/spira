#!/usr/bin/env bash
#
# test-cockpit.sh — the snapshot has exactly one writer.
#
#   ./test-cockpit.sh
#
# THE FAILURE THIS SUITE EXISTS FOR. cockpit.sh writes the snapshot the health
# pane reads, and nothing else may: an aeon running the collector from its
# worktree, a retired brain collector calling a vendored copy, or a manual
# `cockpit.sh once` each overwrites the live snapshot with whatever keys its
# branch carries, and the pane reads `?` for every key the interloper lacked.
#
# The fence is INVOCATION_ID: systemd sets it for exactly one process tree per
# invocation, so the collector refuses to write unless its INVOCATION_ID matches
# spira-cockpit.service's — or SPIRA_COCKPIT_FORCE=1 names the override.
#
# defect: sp-20d
# covers: spira/cockpit.sh
# scar: cockpit.sh wrote the snapshot unconditionally; an aeon running a vendored copy or a manual invocation overwrote the live snapshot, and the pane read `?` for every key the interloper lacked.
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
RUN="$TMP/run"; mkdir -p "$RUN"
BASE_PATH="$PATH"

# A minimal environment: no INVOCATION_ID, no real config, no inherited state.
run_cockpit() {
    env -i PATH="$BASE_PATH" HOME="$TMP" LC_ALL=C.UTF-8 \
        SPIRA_CONF="$TMP/no.conf" SPIRA_HOME="$HERE" SPIRA_REPO="$TMP" \
        SPIRA_RUN="$RUN" SPIRA_DB="$TMP/nodb" \
        SPIRA_REPO_MAP="$TMP/no-map" SPIRA_GOAL=sp-test SPIRA_FAYTHS=t \
        "$@" \
        bash "$HERE/cockpit.sh" once 2>/dev/null
}

# ======================================================================================
echo "without INVOCATION_ID (unsupervised):"

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
# POSITIVE SUPERVISED PATH (gap #8, docs/test-plan/cockpit-observability.md): every case
# above is a refusal. A fence that refused every INVOCATION_ID, including one that legitimately
# matches the service, would pass all of them — this is the only case that would catch that.
echo
echo "with INVOCATION_ID matching the service (supervised):"

BIN="$TMP/bin"; mkdir -p "$BIN"
cat > "$BIN/systemctl" <<'SH'
#!/usr/bin/env bash
# `systemctl --user show <unit> -p InvocationID --value`
case " $* " in
    *" show spira-cockpit.service "*"--value"*)
        echo "$MOCK_INVOCATION_ID"; exit 0 ;;
esac
exit 1
SH
chmod +x "$BIN/systemctl"

rm -f "$RUN/cockpit.env"
env -i PATH="$BASE_PATH" SPIRA_PATH="$BIN" HOME="$TMP" LC_ALL=C.UTF-8 \
    SPIRA_CONF="$TMP/no.conf" SPIRA_HOME="$HERE" SPIRA_REPO="$TMP" \
    SPIRA_RUN="$RUN" SPIRA_DB="$TMP/nodb" \
    SPIRA_REPO_MAP="$TMP/no-map" SPIRA_GOAL=sp-test SPIRA_FAYTHS=t \
    INVOCATION_ID="inv-42" MOCK_INVOCATION_ID="inv-42" \
    bash "$HERE/cockpit.sh" once >/dev/null 2>&1
if [ -f "$RUN/cockpit.env" ]; then
    ok "matching INVOCATION_ID: snapshot IS written"
else
    bad "matching INVOCATION_ID: snapshot IS written" "file does not exist"
fi
snap="$(cat "$RUN/cockpit.env" 2>/dev/null)"
want "SP_WRITER is set"                    "SP_WRITER="                "$snap"
want "SP_WRITER names the real unit"       "spira-cockpit.service"     "$snap"
nowant "SP_WRITER does not say force"      "force"                     "$snap"

# A mismatched INVOCATION_ID against the same, otherwise-working systemctl must still refuse
# — the negative twin, so this case isn't just "any systemctl output writes".
rm -f "$RUN/cockpit.env"
env -i PATH="$BASE_PATH" SPIRA_PATH="$BIN" HOME="$TMP" LC_ALL=C.UTF-8 \
    SPIRA_CONF="$TMP/no.conf" SPIRA_HOME="$HERE" SPIRA_REPO="$TMP" \
    SPIRA_RUN="$RUN" SPIRA_DB="$TMP/nodb" \
    SPIRA_REPO_MAP="$TMP/no-map" SPIRA_GOAL=sp-test SPIRA_FAYTHS=t \
    INVOCATION_ID="inv-imposter" MOCK_INVOCATION_ID="inv-42" \
    bash "$HERE/cockpit.sh" once >/dev/null 2>&1
if [ ! -f "$RUN/cockpit.env" ]; then
    ok "mismatched INVOCATION_ID (same working systemctl): snapshot is NOT written"
else
    bad "mismatched INVOCATION_ID (same working systemctl): snapshot is NOT written" \
        "file exists at $RUN/cockpit.env"
fi

# ======================================================================================
echo
echo "with SPIRA_COCKPIT_FORCE=1:"

rm -f "$RUN/cockpit.env"
run_cockpit env SPIRA_COCKPIT_FORCE=1 >/dev/null
if [ -f "$RUN/cockpit.env" ]; then
    ok "snapshot IS written"
else
    bad "snapshot IS written" "file does not exist"
fi
snap="$(cat "$RUN/cockpit.env" 2>/dev/null)"
want "SP_WRITER is set" "SP_WRITER=" "$snap"
want "SP_WRITER names 'force'" "force" "$snap"

# ======================================================================================
echo
echo "loop mode refuses without supervision:"

loop_out="$(env -i PATH="$BASE_PATH" HOME="$TMP" LC_ALL=C.UTF-8 \
    SPIRA_CONF="$TMP/no.conf" SPIRA_HOME="$HERE" SPIRA_REPO="$TMP" \
    SPIRA_RUN="$RUN" SPIRA_DB="$TMP/nodb" \
    SPIRA_REPO_MAP="$TMP/no-map" SPIRA_GOAL=sp-test SPIRA_FAYTHS=t \
    bash "$HERE/cockpit.sh" loop 2>&1)" || true
want "loop mode prints refusal" "not the supervised process" "$loop_out"

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

rl_out="$(env -i PATH="$BASE_PATH" HOME="$TMP" LC_ALL=C.UTF-8 \
    SPIRA_CONF="$TMP/no.conf" SPIRA_HOME="$HERE" SPIRA_REPO="$TMP" \
    SPIRA_RUN="$RUN" SPIRA_DB="$TMP/nodb" \
    SPIRA_REPO_MAP="$TMP/no-map" SPIRA_GOAL=sp-test SPIRA_FAYTHS=t \
    bash "$HERE/cockpit.sh" ratelim 2>/dev/null)"

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
rl_empty="$(env -i PATH="$BASE_PATH" HOME="$TMP" LC_ALL=C.UTF-8 \
    SPIRA_CONF="$TMP/no.conf" SPIRA_HOME="$HERE" SPIRA_REPO="$TMP" \
    SPIRA_RUN="$EMPTY_RUN" SPIRA_DB="$TMP/nodb" \
    SPIRA_REPO_MAP="$TMP/no-map" SPIRA_GOAL=sp-test SPIRA_FAYTHS=t \
    bash "$HERE/cockpit.sh" ratelim 2>/dev/null)"

want "SP_RATELIM_5H is '?'" "SP_RATELIM_5H=?" "$rl_empty"
want "SP_RATELIM_7D is '?'" "SP_RATELIM_7D=?" "$rl_empty"
want "SP_RATELIM_5H_PCT is '?'" "SP_RATELIM_5H_PCT=?" "$rl_empty"

# ======================================================================================
echo
printf 'test-cockpit: %d ok, %d fail\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
