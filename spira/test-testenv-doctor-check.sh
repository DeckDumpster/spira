#!/usr/bin/env bash
#
# test-testenv-doctor-check.sh — doctor-check.sh enforces doctor.sh's program lists.
#
# WHAT THIS TESTS
# ---------------
# 1. POSITIVE CONTROL (can find failures): a doctor.sh with a FATAL program not on PATH
#    causes doctor-check.sh to exit non-zero. This proves the checker fires; without it,
#    passing runs are indistinguishable from the checker being absent.
# 2. BOGUS FATAL fails: adding a program that does not exist to the FATAL loop causes
#    failure (the primary acceptance criterion: "adding a bogus program to doctor.sh's
#    FATAL list fails the image build").
# 3. BOGUS WARN without waiver fails: a WARN-level absent program with no waiver is refused.
# 4. WAIVER with reason passes: a WARN-level absent program with a waiver line is accepted.
# 5. EMPTY WAIVER is refused: a waiver line with no reason text is an error, not a pass.
# 6. CLEAN RUN passes: a doctor.sh whose listed programs are all present exits 0.
#
# SKIP CONDITION: none. Only bash and the filesystem are needed.
#
# defect: sp-qwmj
# covers: spira/testenv/doctor-check.sh spira/testenv/Containerfile spira/doctor.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
isnz()   { [ "$2" -ne 0 ] && ok "$1" || bad "$1" "expected exit non-zero, got 0"; }
iszero() { [ "$2" -eq 0 ] && ok "$1" || bad "$1" "expected exit 0, got $2"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in output [$3]"; }

echo "test-testenv-doctor-check.sh"

CHECKER="$HERE/testenv/doctor-check.sh"

# ── Pre-check: doctor-check.sh must exist ────────────────────────────────────
# This is the assertion that fails against the tree without the fix: the script is
# absent, so every test below would produce a wrong result. Reporting it as a
# failure here makes the "before" run legible (law-absence-needs-a-positive-control).
if [ ! -f "$CHECKER" ]; then
    bad "doctor-check.sh exists" "not found at $CHECKER (positive control: fails before the fix)"
    printf '\n%d passed, %d failed\n' "$pass" "$fail"
    exit 1
fi

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM

# Helper: create a minimal doctor.sh stub with configurable program loops.
# The stub contains only the two `for b in` loops, enough for doctor-check.sh
# to parse it. The loop format must match doctor.sh's actual format so the awk
# parser (which anchors on `^for b in .*; do$`) fires correctly.
write_doctor() {
    local fatal="$1" warn="$2"
    # NOTE: the `do` must be at the end of the line (^for b in ...; do$) to
    # match doctor-check.sh's awk pattern, which is the same format doctor.sh uses.
    printf '%s\n' "#!/usr/bin/env bash" \
                  "# Minimal doctor.sh stub for testing." \
                  "for b in ${fatal}; do" \
                  "    true" \
                  "done" \
                  "for b in ${warn}; do" \
                  "    true" \
                  "done" > "$TMP/doctor.sh"
}

# Resolve a program that is guaranteed NOT to be on PATH.
ABSENT="doctor-check-nonexistent-xyzzy-$$"

# ============================================================================
echo
echo "positive control — checker exits non-zero when a FATAL is absent:"
# ============================================================================
# Without this, a "clean run" pass is indistinguishable from the checker
# silently exiting 0 regardless of the programs it finds.
write_doctor "$ABSENT" "cargo"
out="$(bash "$CHECKER" "$TMP/doctor.sh" 2>&1)"; rc=$?
isnz "positive control: exits non-zero on absent FATAL" "$rc"
want "positive control: names the absent program" "$ABSENT" "$out"

# ============================================================================
echo
echo "bogus FATAL — a program not on PATH in the FATAL loop fails:"
# ============================================================================
write_doctor "bash $ABSENT" "cargo"
out="$(bash "$CHECKER" "$TMP/doctor.sh" 2>&1)"; rc=$?
isnz "bogus FATAL: exits non-zero" "$rc"
want "bogus FATAL: names the missing program" "$ABSENT" "$out"

# ============================================================================
echo
echo "absent WARN without waiver — fails:"
# ============================================================================
write_doctor "bash" "$ABSENT"
out="$(bash "$CHECKER" "$TMP/doctor.sh" 2>&1)"; rc=$?
isnz "absent WARN without waiver: exits non-zero" "$rc"
want "absent WARN without waiver: names the program" "$ABSENT" "$out"

# ============================================================================
echo
echo "absent WARN with waiver+reason — passes:"
# ============================================================================
write_doctor "bash" "$ABSENT"
printf '%s  program is intentionally absent from this image — test waiver\n' "$ABSENT" \
    > "$TMP/waivers"
out="$(bash "$CHECKER" "$TMP/doctor.sh" "$TMP/waivers" 2>&1)"; rc=$?
iszero "absent WARN with waiver+reason: exits 0" "$rc"
want "absent WARN with waiver+reason: shows waived" "waived" "$out"

# ============================================================================
echo
echo "empty waiver — refused even if program is absent:"
# ============================================================================
write_doctor "bash" "$ABSENT"
# Waiver line has program name but no reason text.
printf '%s\n' "$ABSENT" > "$TMP/empty-waivers"
out="$(bash "$CHECKER" "$TMP/doctor.sh" "$TMP/empty-waivers" 2>&1)"; rc=$?
isnz "empty waiver: exits non-zero" "$rc"
want "empty waiver: error mentions 'reason'" "reason" "$out"

# ============================================================================
echo
echo "clean run — all programs present exits 0:"
# ============================================================================
# Use programs that are known to be on PATH: bash and python3.
write_doctor "bash python3" "git"
out="$(bash "$CHECKER" "$TMP/doctor.sh" 2>&1)"; rc=$?
iszero "clean run: exits 0" "$rc"
want "clean run: mentions 'present or waived'" "present or waived" "$out"

# ============================================================================
echo
echo "real doctor.sh — programs against actual doctor.sh with waiver file:"
# ============================================================================
# Run doctor-check.sh against the real doctor.sh with the real waivers file.
# This catches any divergence between the stub tests above and what the actual
# Containerfile does. It will fail if doctor-check.sh cannot parse doctor.sh's
# actual loop format.
REAL_DOCTOR="$HERE/doctor.sh"
REAL_WAIVERS="$HERE/testenv/doctor-waivers"
if [ ! -f "$REAL_WAIVERS" ]; then
    bad "real waivers file exists" "not found at $REAL_WAIVERS"
else
    real_out="$(bash "$CHECKER" "$REAL_DOCTOR" "$REAL_WAIVERS" 2>&1)"; real_rc=$?
    # The check passes only when every program is present OR waived. On this host,
    # all of doctor.sh's FATAL programs should be present (bd, git, python3, flock).
    # WARN programs should be present (dolt, gh, tmux, cargo, node) or waived (claude).
    iszero "real doctor.sh with real waivers: exits 0" "$real_rc"
    [ $real_rc -ne 0 ] && printf '%s\n' "$real_out" >&2
fi

# ============================================================================
echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" = 0 ]
