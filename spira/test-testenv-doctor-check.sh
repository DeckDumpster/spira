#!/usr/bin/env bash
#
# test-testenv-doctor-check.sh — doctor-check.sh enforces conf.sh's dependency manifest.
#
# WHAT THIS TESTS
# ---------------
# 1. POSITIVE CONTROL (can find failures): a manifest with a runtime program not on PATH
#    causes doctor-check.sh to exit non-zero. This proves the checker fires; without it,
#    passing runs are indistinguishable from the checker being absent.
# 2. BOGUS RUNTIME fails: a runtime-tier program that does not exist fails the build.
# 3. BOGUS OPTIONAL/OPERATOR without waiver fails: absent and unwaived is refused.
# 4. WAIVER with reason passes: an absent optional/operator program with a waiver line is
#    accepted.
# 5. EMPTY WAIVER is refused: a waiver line with no reason text is an error, not a pass.
# 6. DEV TIER is never checked: an absent dev-tier program is neither FAIL nor waived-for.
# 7. CLEAN RUN passes: a manifest whose programs are all present exits 0.
# 8. REAL MANIFEST — doctor-check.sh against the real conf.sh with the real waivers file
#    exits 0 on this host, catching any divergence between the stub cases above and what
#    the actual Containerfile runs.
#
# SKIP CONDITION: none. Only bash and the filesystem are needed.
#
# defect: sp-qwmj (this suite was rewritten for sp-utt1i: doctor-check.sh reads conf.sh's
# deps.toml manifest now, not doctor.sh's own program loops — doctor.sh no longer carries
# build-input checks at all)
# tier: T1
# covers: spira/testenv/doctor-check.sh spira/testenv/Containerfile spira/conf.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"
isnz()   { [ "$2" -ne 0 ] && ok "$1" || bad "$1" "expected exit non-zero, got 0"; }
iszero() { [ "$2" -eq 0 ] && ok "$1" || bad "$1" "expected exit 0, got $2"; }

echo "test-testenv-doctor-check.sh"

CHECKER="$HERE/testenv/doctor-check.sh"

if [ ! -f "$CHECKER" ]; then
    bad "doctor-check.sh exists" "not found at $CHECKER"
    tl_summary; exit 1
fi

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM

# Helper: a minimal conf.sh stub declaring spira_deps_list and spira_bin_tier, enough for
# doctor-check.sh to build its FATAL/WARN sets from.
write_conf() {
    local runtime="$1" optional="$2" dev="${3:-}"
    cat > "$TMP/conf.sh" <<EOF
spira_deps_list() { printf '%s\n' ${runtime} ${optional} ${dev}; }
spira_bin_tier() {
    case "\$1" in
$(for p in $runtime; do printf '        %s) echo runtime ;;\n' "$p"; done)
$(for p in $optional; do printf '        %s) echo optional ;;\n' "$p"; done)
$(for p in $dev; do printf '        %s) echo dev ;;\n' "$p"; done)
        *) echo optional ;;
    esac
}
EOF
}

ABSENT="doctor-check-nonexistent-xyzzy-$$"

# ============================================================================
echo
echo "positive control — checker exits non-zero when a runtime program is absent:"
# ============================================================================
write_conf "$ABSENT" "cargo"
out="$(bash "$CHECKER" "$TMP/conf.sh" 2>&1)"; rc=$?
isnz "positive control: exits non-zero on absent runtime program" "$rc"
want "positive control: names the absent program" "$ABSENT" "$out"

# ============================================================================
echo
echo "bogus runtime — a program not on PATH in the runtime tier fails:"
# ============================================================================
write_conf "bash $ABSENT" "cargo"
out="$(bash "$CHECKER" "$TMP/conf.sh" 2>&1)"; rc=$?
isnz "bogus runtime: exits non-zero" "$rc"
want "bogus runtime: names the missing program" "$ABSENT" "$out"

# ============================================================================
echo
echo "absent optional without waiver — fails:"
# ============================================================================
write_conf "bash" "$ABSENT"
out="$(bash "$CHECKER" "$TMP/conf.sh" 2>&1)"; rc=$?
isnz "absent optional without waiver: exits non-zero" "$rc"
want "absent optional without waiver: names the program" "$ABSENT" "$out"

# ============================================================================
echo
echo "absent optional with waiver+reason — passes:"
# ============================================================================
write_conf "bash" "$ABSENT"
printf '%s  program is intentionally absent from this image — test waiver\n' "$ABSENT" \
    > "$TMP/waivers"
out="$(bash "$CHECKER" "$TMP/conf.sh" "$TMP/waivers" 2>&1)"; rc=$?
iszero "absent optional with waiver+reason: exits 0" "$rc"
want "absent optional with waiver+reason: shows waived" "waived" "$out"

# ============================================================================
echo
echo "empty waiver — refused even if program is absent:"
# ============================================================================
write_conf "bash" "$ABSENT"
printf '%s\n' "$ABSENT" > "$TMP/empty-waivers"
out="$(bash "$CHECKER" "$TMP/conf.sh" "$TMP/empty-waivers" 2>&1)"; rc=$?
isnz "empty waiver: exits non-zero" "$rc"
want "empty waiver: error mentions 'reason'" "reason" "$out"

# ============================================================================
echo
echo "dev tier — an absent dev-tier program is never checked:"
# ============================================================================
write_conf "bash" "git" "$ABSENT"
out="$(bash "$CHECKER" "$TMP/conf.sh" 2>&1)"; rc=$?
iszero "dev tier: absent dev program does not fail the build" "$rc"
nowant "dev tier: absent dev program is not named" "$ABSENT" "$out"

# ============================================================================
echo
echo "clean run — all programs present exits 0:"
# ============================================================================
write_conf "bash python3" "git"
out="$(bash "$CHECKER" "$TMP/conf.sh" 2>&1)"; rc=$?
iszero "clean run: exits 0" "$rc"
want "clean run: mentions 'present or waived'" "present or waived" "$out"

# ============================================================================
echo
echo "real manifest — conf.sh's actual deps.toml with the real waivers file:"
# ============================================================================
REAL_CONF="$HERE/conf.sh"
REAL_WAIVERS="$HERE/testenv/doctor-waivers"
if [ ! -f "$REAL_WAIVERS" ]; then
    bad "real waivers file exists" "not found at $REAL_WAIVERS"
else
    real_out="$(bash "$CHECKER" "$REAL_CONF" "$REAL_WAIVERS" 2>&1)"; real_rc=$?
    iszero "real manifest with real waivers: exits 0" "$real_rc"
    [ $real_rc -ne 0 ] && printf '%s\n' "$real_out" >&2
fi

# ============================================================================
echo
tl_summary
