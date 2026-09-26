#!/usr/bin/env bash
# test-batch-peak.sh — cgroup peak WARNING fires when it drifts toward MemTotal
#
# WHAT THIS PROVES
#   1. testenv-batch.sh compares the logged cgroup peak against a fraction of MemTotal
#      and warns when the peak is over it — the check the "cgroup peak" log line was
#      logged for and nothing ever read (a template ran 3.7x its measured high-water
#      mark undetected for months).
#   2. SPIRA_BATCH_PEAK_WARN_FRAC=0 disables the check.
#   3. SPIRA_BATCH_PEAK_WARN_FRAC is in SPIRA_CONF_KEYS.
#
# POSITIVE CONTROLS (law-a-regression-test-must-be-seen-to-fail)
#   A1 plants a peak just under the threshold and requires no WARNING; A2 plants one
#   just over it and requires the WARNING text. If the comparison is deleted or
#   inverted, one of the two goes silent or fires. B's positive control asserts a
#   fabricated key is absent from SPIRA_CONF_KEYS.
#
# covers: spira/testenv-batch.sh spira/conf.sh

set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"

pass=0; fail=0
ok()      { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()     { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want()    { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
notwant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

BATCH="$HERE/testenv-batch.sh"
CONF_SH="$HERE/conf.sh"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

echo "test-batch-peak.sh"

# ===========================================================================
# PART A: WARNING fires only once the peak crosses the declared fraction
# ===========================================================================
echo
echo "Part A: peak-vs-MemTotal WARNING"

_block="$(sed -n '/#!peak-begin/,/#!peak-end/{/#!peak-/d; p}' "$BATCH")"
[ -n "$_block" ] || {
    printf '  FAIL  A0: peak check block not found in %s\n' "$BATCH" >&2
    exit 1
}
ok "A0: peak check block found"

FAKE_MEMINFO="$TMP/meminfo"
# 8 GiB total (8388608 kB) — non-default, chosen so 60% ceiling is 4915 MiB.
printf 'MemTotal:       8388608 kB\nMemAvailable:   4000000 kB\n' > "$FAKE_MEMINFO"

_run_peak() {
    local _peak_mib="$1" _frac="$2"
    bash -c "
        log() { printf '%s\n' \"\$1\"; }
        awk() { command awk \"\$1\" '$FAKE_MEMINFO'; }
        _peak_mib=$_peak_mib
        SPIRA_BATCH_PEAK_WARN_FRAC=$_frac
        $_block
    "
}

# 60% of 8192 MiB (8388608 kB / 1024) is 4915 MiB.
_out_under="$(_run_peak 4900 60)"
notwant "A1: peak just under 60% ceiling → no WARNING" "WARNING" "$_out_under"

_out_over="$(_run_peak 5000 60)"
want "A2: peak just over 60% ceiling → WARNING fires" "WARNING" "$_out_over"
want "A3: WARNING names the peak" "5000MiB" "$_out_over"
want "A4: WARNING names MemTotal" "8192MiB" "$_out_over"

_out_disabled="$(_run_peak 8000 0)"
notwant "A5: SPIRA_BATCH_PEAK_WARN_FRAC=0 disables the check" "WARNING" "$_out_disabled"

# ===========================================================================
# PART B: new key is accepted by conf.sh's allowlist
# ===========================================================================
echo
echo "Part B: new conf key in SPIRA_CONF_KEYS"

_conf_keys="$(
    SPIRA_HOME="$HERE" \
    SPIRA_CONF=/nonexistent \
    bash -c ". '$CONF_SH'; printf '%s' \"\$SPIRA_CONF_KEYS\""
)"

want "B-SPIRA_BATCH_PEAK_WARN_FRAC" "SPIRA_BATCH_PEAK_WARN_FRAC" "$_conf_keys"
notwant "B-pos: SPIRA_BATCH_PEAK_NOEXIST absent (positive control)" \
        "SPIRA_BATCH_PEAK_NOEXIST" "$_conf_keys"

# ===========================================================================
echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ] || exit 1
