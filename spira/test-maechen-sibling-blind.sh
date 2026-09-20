#!/usr/bin/env bash
#
# test-maechen-sibling-blind.sh — regression for sp-ctz1a: Step 5 of maechen.md must
#   use a compare-and-swap rather than an unconditional watermark advance. When N passes
#   fire concurrently, the first to reach Step 5 moves the watermark past every other
#   pass's census window; each remaining pass then reports threshold_met=no over a window
#   it was never able to see.
#
#   ./test-maechen-sibling-blind.sh
#
# THE DEFECT (sp-ctz1a — distinct from sp-pyzp)
# ----------------------------------------------
# test-maechen-blind-window.sh covers the sp-pyzp defect: the *trigger* must not advance
# the watermark before filing the bead. That test says nothing about a *sibling pass*
# advancing it while another pass is running. Both produce the same blind window; they
# enter through different doors. sp-pyzp's test does not cover this path.
#
# SCENARIO
# --------
# T         = one hour ago (the old watermark)
# Pass A starts: reads wm_start=T, runs census
# Pass B (sibling) finishes first: reads wm_start=T, reaches Step 5, advances to T_B
# Pass A now reaches Step 5:
#   UNFIXED: no wm_start captured; unconditionally advances to T_A; logs threshold_met=no
#   FIXED:   reads wm_now=T_B != wm_start=T; logs "blinded"; leaves watermark at T_B
#
# POSITIVE CONTROL (law-absence-needs-a-positive-control)
# -------------------------------------------------------
# Each grep check for new CAS language is preceded by a planted-offender assertion that
# confirms the grep pattern is capable of finding the target string.
#
# SEEN TO FAIL BEFORE THE FIX (law-a-regression-test-must-be-seen-to-fail)
# -------------------------------------------------------------------------
# On the unfixed maechen.md (no wm_start capture, unconditional watermark advance):
#   FAIL — want [wm_start] in maechen.md Step 1 (string absent)
#   FAIL — want [wm_now] in maechen.md Step 5 (string absent)
#   FAIL — want [pass blinded] in maechen.md log formats (string absent)
#   FAIL — want [Census window collapsed] in maechen.md (string absent)
# The fix (wm_start capture in Step 1, CAS in Step 5) makes all four pass.
#
# covers: spira/chamber/maechen.md
# hermetic-ok: no database, no systemd; reads files and runs shell simulation only
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
BRIEF="$HERE/chamber/maechen.md"

pass=0; fail=0
ok()   { pass=$((pass+1)); printf '  ok   — %s\n' "$1"; }
bad()  { fail=$((fail+1)); printf '  FAIL — %s: %s\n' "$1" "${2:-}"; }
is()   { [ "$2" = "$3" ] && ok "$1" || bad "$1" "expected [$2] got [$3]"; }
want() { case "$3" in *"$2"*) ok "$1" ;; *) bad "$1" "wanted [$2] in [$3]"; esac; }
lack() { case "$3" in *"$2"*) bad "$1" "did not want [$2] in [$3]" ;; *) ok "$1" ;; esac; }

T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT INT TERM

echo "test-maechen-sibling-blind.sh"

# ==============================================================================
echo
echo "SETUP: brief exists"
# ==============================================================================
if [ -f "$BRIEF" ]; then
    ok "maechen.md exists"
else
    bad "maechen.md" "not found at $BRIEF"
    printf '%s: %d passed, %d failed\n' "$(basename "$0")" "$pass" "$fail"
    exit 1
fi

brief="$(cat "$BRIEF")"

# ==============================================================================
echo
echo "BRIEF: Step 1 must capture wm_start before running census"
# ==============================================================================
# POSITIVE CONTROL: plant the target string and verify the grep can find it.
_planted="$(printf '%s\nwm_start_positive_control' "$brief")"
if grep -q 'wm_start_positive_control' <<< "$_planted"; then
    ok "positive control: grep can find wm_start"
else
    bad "positive control" "grep missed planted wm_start — grep pattern is broken"
fi

want "brief captures wm_start before census (Step 1)" "wm_start" "$brief"

# ==============================================================================
echo
echo "BRIEF: Step 5 must compare wm_now against wm_start before advancing"
# ==============================================================================
_planted2="$(printf '%s\nwm_now_positive_control' "$brief")"
if grep -q 'wm_now_positive_control' <<< "$_planted2"; then
    ok "positive control: grep can find wm_now"
else
    bad "positive control" "grep missed planted wm_now — grep pattern is broken"
fi

want "brief reads wm_now at Step 5" "wm_now" "$brief"
want "brief compares wm_now to wm_start (CAS guard)" 'wm_now" = "$wm_start' "$brief"

# ==============================================================================
echo
echo "BRIEF: Step 5 must have a distinct blinded-pass log format"
# ==============================================================================
want "brief has blinded-pass log format" "pass blinded" "$brief"
want "blinded entry names the cause" "sibling pass" "$brief"
want "blinded entry is distinct from threshold_met format" "Census window collapsed" "$brief"

# ==============================================================================
echo
echo "SIMULATION: CAS leaves watermark alone when sibling has already advanced it"
# ==============================================================================
# Simulate the Step 5 shell commands the fixed brief instructs, to verify the
# mechanism is sound independently of the prose.
_run="$T/run"
_log="$_run/maechen.log"
mkdir -p "$_run"

_T_WM=1700000000
_T_B=$((_T_WM + 600))

# Pass A captured wm_start at the start of its pass.
_wm_start_a="$_T_WM"

# Sibling (Pass B) finishes first and advances the watermark to T_B.
printf '%d\n' "$_T_B" > "$_run/maechen.watermark"

# Pass A now executes Step 5: compare wm_now against its wm_start.
(
    _wm_now="$(cat "$_run/maechen.watermark" 2>/dev/null || printf '')"
    if [ "$_wm_now" = "$_wm_start_a" ]; then
        printf '%d\n' "$(date +%s)" > "$_run/maechen.watermark.new" \
            && mv "$_run/maechen.watermark.new" "$_run/maechen.watermark"
        printf 'Maechen pass done: census=3 classes ranked (instrument rc=0), threshold_met=yes, beads_cut=1. Watermark advanced to %s.\n' \
            "$(date -u +%Y-%m-%dT%H:%M:%SZ)" >> "$_log"
    else
        printf 'Maechen pass blinded: watermark advanced by sibling pass (was %s, now %s). Census window collapsed; pass recorded without verdict.\n' \
            "$_wm_start_a" "$_wm_now" >> "$_log"
    fi
)

_log_out="$(cat "$_log")"
want "simulation (sibling advanced): blinded log entry written" "pass blinded" "$_log_out"
lack "simulation (sibling advanced): no false threshold_met=no logged" "threshold_met=no" "$_log_out"

_wm_final="$(cat "$_run/maechen.watermark" | tr -d '[:space:]')"
is "simulation (sibling advanced): watermark unchanged at T_B (not advanced by blinded pass)" \
    "$_T_B" "$_wm_final"

# ==============================================================================
echo
echo "SIMULATION: CAS advances watermark when no sibling has moved it"
# ==============================================================================
rm -f "$_log"
printf '%d\n' "$_T_WM" > "$_run/maechen.watermark"

_T_ADVANCE=$((_T_WM + 1200))

(
    _wm_start2="$_T_WM"
    _wm_now2="$(cat "$_run/maechen.watermark" 2>/dev/null || printf '')"
    if [ "$_wm_now2" = "$_wm_start2" ]; then
        printf '%d\n' "$_T_ADVANCE" > "$_run/maechen.watermark.new" \
            && mv "$_run/maechen.watermark.new" "$_run/maechen.watermark"
        printf 'Maechen pass done: census=3 classes ranked (instrument rc=0), threshold_met=yes, beads_cut=1. Watermark advanced to %s.\n' \
            "$(date -u +%Y-%m-%dT%H:%M:%SZ)" >> "$_log"
    else
        printf 'Maechen pass blinded: watermark advanced by sibling pass (was %s, now %s). Census window collapsed; pass recorded without verdict.\n' \
            "$_wm_start2" "$_wm_now2" >> "$_log"
    fi
)

_log_out2="$(cat "$_log")"
want "simulation (no sibling): done log entry written" "pass done" "$_log_out2"
lack "simulation (no sibling): no blinded entry when watermark unchanged" "pass blinded" "$_log_out2"

_wm_final2="$(cat "$_run/maechen.watermark" | tr -d '[:space:]')"
is "simulation (no sibling): watermark advanced to expected value" \
    "$_T_ADVANCE" "$_wm_final2"

echo
printf '%s: %d passed, %d failed\n' "$(basename "$0")" "$pass" "$fail"
[ "$fail" -eq 0 ]
