#!/usr/bin/env bash
#
# test-bin-manifest.sh — every program the harness needs is declared, tiered, and checked
#
#   ./test-bin-manifest.sh
#
# THE DEFECT THIS SUITE GUARDS AGAINST. "Dependency" used to mean one undifferentiated
# thing. doctor.sh carried its own two hardcoded lists — one fatal, one warning — and
# conf.sh carried a separate purpose table, so a program could be needed by the harness and
# named by neither. That is what happened to bd-embedded: the fixture engine for every test
# suite, absent from the box for a week, reported by nothing, while the fixture builder
# silently fell back to a shared Dolt server and the resulting failures read as defects in
# the code under test.
#
# THREE PROPERTIES, and the third is the one that was missing:
#   1. Every declared program has a purpose and a tier — no silent entries.
#   2. doctor.sh checks every declared program — the manifest and the checker cannot drift,
#      which is the two-lists defect above.
#   3. A program whose absence DOWNGRADES behaviour rather than stopping it declares what
#      that downgrade is. Tier alone does not capture it: bd-embedded is 'dev', but its
#      absence is not "tests off", it is "tests run on a much worse engine, quietly".
#
# POSITIVE CONTROL (law-absence-needs-a-positive-control). Each assertion that scans for an
# omission first plants one and requires the scan to report it. A scan over a list that
# failed to load reports no omissions and reads exactly like a clean bill of health.
#
# covers: spira/conf.sh spira/doctor.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()  { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }

echo "test-bin-manifest.sh"

# An explicit, minimal environment: a real spira.conf on this box must not decide a verdict
# (law-gates-run-in-a-clean-environment). SPIRA_CONF names a file that does not exist.
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
eval "$(env -i HOME="$HOME" PATH="$PATH" SPIRA_CONF="$TMP/none.conf" bash -c '
    . '"$HERE"'/conf.sh 2>/dev/null
    _manifest="$(spira_deps_list)"
    printf "MANIFEST=%q\n" "$_manifest"
    for b in $_manifest; do
        printf "TIER_%s=%q\n"     "${b//-/_}" "$(spira_bin_tier    "$b")"
        printf "PURPOSE_%s=%q\n"  "${b//-/_}" "$(spira_bin_purpose "$b")"
        printf "ABSENT_%s=%q\n"   "${b//-/_}" "$(spira_bin_absent  "$b")"
        printf "VPROBE_%s=%q\n"   "${b//-/_}" "$(
            python3 - '"$HERE"'/deps.toml "$b" 2>/dev/null <<'"'"'_PY'"'"'
import sys, tomllib
with open(sys.argv[1], "rb") as f:
    data = tomllib.load(f)
for d in data.get("dep", []):
    if d["name"] == sys.argv[2]:
        print(d.get("version_probe", ""))
        break
_PY
        )"
    done
')"

# ======================================================================================
echo
echo "POSITIVE CONTROL — the manifest actually loaded:"
# ======================================================================================
# Everything below scans the manifest. An empty one passes every "no bad entries" check
# while proving nothing at all.
_n=0; for _b in ${MANIFEST:-}; do _n=$((_n+1)); done
[ "$_n" -ge 8 ] && ok "manifest loaded ($_n programs)" \
                || bad "manifest loaded" "only $_n programs — deps.toml did not load"

# ======================================================================================
echo
echo "every declared program has a tier and a purpose:"
# ======================================================================================
_untiered=""; _unpurposed=""
for _b in ${MANIFEST:-}; do
    _k="${_b//-/_}"; _t="TIER_$_k"; _p="PURPOSE_$_k"
    case "${!_t:-}" in runtime|optional|dev|operator) ;; *) _untiered="$_untiered $_b" ;; esac
    case "${!_p:-}" in ''|'required by the harness') _unpurposed="$_unpurposed $_b" ;; esac
done
is "every program has a valid tier"  "" "$_untiered"
is "every program states a purpose"  "" "$_unpurposed"

# Positive control for the two scans above: an undeclared name must come back untiered and
# with the fallback purpose, or the scans are not looking at anything.
_probe_t="$(env -i HOME="$HOME" PATH="$PATH" SPIRA_CONF="$TMP/none.conf" bash -c \
    ". $HERE/conf.sh 2>/dev/null; spira_bin_tier spira-no-such-program")"
_probe_p="$(env -i HOME="$HOME" PATH="$PATH" SPIRA_CONF="$TMP/none.conf" bash -c \
    ". $HERE/conf.sh 2>/dev/null; spira_bin_purpose spira-no-such-program")"
is "an undeclared program falls back to 'optional'" "optional" "$_probe_t"
is "an undeclared program has no real purpose" "required by the harness" "$_probe_p"

# ======================================================================================
echo
echo "doctor.sh checks every program the manifest declares:"
# ======================================================================================
# THE TWO-LISTS DEFECT. doctor.sh once carried its own copy of what to check, so a program
# could be declared here and examined by nothing. Every declared name must appear in
# doctor.sh — either in one of its explicit loops or via the manifest iteration.
_unchecked=""
for _b in ${MANIFEST:-}; do
    grep -q -- "$_b" "$HERE/doctor.sh" 2>/dev/null || {
        # The manifest-driven loop covers all dev-tier programs via spira_deps_list.
        _k="${_b//-/_}"; _t="TIER_$_k"
        [ "${!_t:-}" = dev ] && grep -q 'spira_deps_list dev' "$HERE/doctor.sh" && continue
        _unchecked="$_unchecked $_b"
    }
done
is "no declared program is unchecked by doctor.sh" "" "$_unchecked"

# ======================================================================================
echo
echo "every declared program has a version probe (T1):"
# ======================================================================================
# A manifest entry with no version_probe cannot satisfy T1. Every dep must declare HOW
# to read its version so a future version-check can run the probe.
_noprobe=""
for _b in ${MANIFEST:-}; do
    _k="${_b//-/_}"; _v="VPROBE_$_k"
    [ -n "${!_v:-}" ] || _noprobe="$_noprobe $_b"
done
is "every program has a version probe" "" "$_noprobe"

# ======================================================================================
echo
echo "a silent downgrade declares what it downgrades to:"
# ======================================================================================
# THE PROPERTY THAT WAS MISSING. bd-embedded is the case that cost a week: absent, it does
# not stop the tests, it moves them onto a shared Dolt server measured at 5-of-6 concurrent
# failures. An operator cannot infer that from "binary not found", so the consequence is
# declared and doctor.sh prints it in every mode.
_e="ABSENT_bd_embedded"
[ -n "${!_e:-}" ] && ok "bd-embedded declares its absence consequence" \
                  || bad "bd-embedded declares its absence consequence" \
                         "spira_bin_absent bd-embedded is empty — a silent fallback with nothing said about it"

case "${!_e:-}" in
    *"shared Dolt server"*) ok "the consequence names the engine it falls back to" ;;
    *) bad "the consequence names the engine it falls back to" "got [${!_e:-<empty>}]" ;;
esac

# doctor.sh must actually print it, not merely have it available.
grep -q 'spira_bin_absent' "$HERE/doctor.sh" \
    && ok "doctor.sh prints the absence consequence" \
    || bad "doctor.sh prints the absence consequence" "doctor.sh never calls spira_bin_absent"

# ======================================================================================
echo
echo "operator tier — four tools that are FAIL when absent on an operated instance:"
# ======================================================================================
# POSITIVE CONTROL (law-absence-needs-a-positive-control). hunk is not installed in the
# testenv container (it is waived in doctor-waivers as an operator tool that no suite
# invokes). Running doctor.sh in this environment with SPIRA_OPERATED=1 and hunk in
# COCKPIT_SESSIONS must produce FAIL output, not 'warn'. Before this fix, hunk was in the
# WARN loop and produced 'warn' — the test was red, which is the required state before a
# regression fix (law-a-regression-test-must-be-seen-to-fail).
#
# inotifywait is installed in this container (test-mail-deliver.sh uses the real binary),
# so the positive control uses hunk, and the tier check covers the others.

# Tier: all four operator tools
for _b in inotifywait aerc hunk go; do
    _k="${_b//-/_}"
    _t_var="TIER_$_k"
    is "$_b is operator tier" "operator" "${!_t_var:-}"
done

# SPIRA_OPERATED setting: 1 is the default (operated instance).
_op_default="$(env -i HOME="$HOME" PATH="$PATH" SPIRA_CONF="$TMP/none.conf" bash -c \
    ". $HERE/conf.sh 2>/dev/null; printf '%s' \"\${SPIRA_OPERATED}\"" 2>/dev/null)"
is "SPIRA_OPERATED defaults to 1" "1" "$_op_default"

# Stubs live under $TMP, never under $HOME. SPIRA_PATH="$STUB_BIN" puts the stub dir
# first in the PATH conf.sh builds, ahead of $HOME/.local/bin, so a real installed binary
# is never touched. Writing to $HOME/.local/bin follows symlinks and can truncate the real
# target — that is what destroyed the operator's hunk installation (sp-jy2d1).
STUB_BIN="$TMP/stub-bin"
mkdir -p "$STUB_BIN"
# Guard: fail immediately if the stub dir is not inside $TMP.
case "$STUB_BIN" in
    "$TMP"/*|"$TMP") ;;
    *) bad "guard: stub dir is inside TMP" \
           "stub dir $STUB_BIN escapes TMP=$TMP — refusing to write stubs"; exit 1 ;;
esac

# POSITIVE CONTROL: with hunk on PATH, operator section says ok.
# Stub is a minimal shell script (not a symlink): `command -v true` returns the builtin
# name, not an absolute path, so a symlink to it would be broken.
printf '#!/bin/sh\n' > "$STUB_BIN/hunk" && chmod +x "$STUB_BIN/hunk"
_pc_out="$(SPIRA_CONF="$TMP/none.conf" SPIRA_DOCTOR_INSTALLING=1 \
    SPIRA_OPERATED=1 COCKPIT_SESSIONS="brain hunk chat" \
    SPIRA_PATH="$STUB_BIN" \
    bash "$HERE/doctor.sh" 2>&1 || true)"
rm -f "$STUB_BIN/hunk"
printf '%s\n' "$_pc_out" | grep -q 'ok.*hunk' \
    && ok "POSITIVE CONTROL: hunk stub present → ok" \
    || bad "POSITIVE CONTROL: hunk stub present → ok" \
           "$(printf '%s\n' "$_pc_out" | grep -i hunk | head -2)"

# hunk absent, SPIRA_OPERATED=1 → FAIL (not warn).
_op_out="$(SPIRA_CONF="$TMP/none.conf" SPIRA_DOCTOR_INSTALLING=1 \
    SPIRA_OPERATED=1 COCKPIT_SESSIONS="brain hunk chat" \
    SPIRA_PATH="$STUB_BIN" \
    bash "$HERE/doctor.sh" 2>&1 || true)"
printf '%s\n' "$_op_out" | grep -q 'FAIL.*hunk' \
    && ok "hunk absent with SPIRA_OPERATED=1 → FAIL" \
    || bad "hunk absent with SPIRA_OPERATED=1 → FAIL" \
           "$(printf '%s\n' "$_op_out" | grep -i hunk | head -2)"

# hunk absent, SPIRA_OPERATED=0 → warn (not FAIL).
_op_out2="$(SPIRA_CONF="$TMP/none.conf" SPIRA_DOCTOR_INSTALLING=1 \
    SPIRA_OPERATED=0 COCKPIT_SESSIONS="brain hunk chat" \
    SPIRA_PATH="$STUB_BIN" \
    bash "$HERE/doctor.sh" 2>&1 || true)"
printf '%s\n' "$_op_out2" | grep -q '  warn  hunk' \
    && ok "hunk absent with SPIRA_OPERATED=0 → warn" \
    || bad "hunk absent with SPIRA_OPERATED=0 → warn" \
           "$(printf '%s\n' "$_op_out2" | grep -i hunk | head -2)"

# COCKPIT_MAIL: checked as configured client, not literally aerc.
# With COCKPIT_MAIL=mutt and mutt present, aerc absence must not produce FAIL.
printf '#!/bin/sh\n' > "$STUB_BIN/mutt" && chmod +x "$STUB_BIN/mutt"
_mail_out="$(SPIRA_CONF="$TMP/none.conf" SPIRA_DOCTOR_INSTALLING=1 \
    SPIRA_OPERATED=1 COCKPIT_MAIL=mutt \
    SPIRA_PATH="$STUB_BIN" \
    bash "$HERE/doctor.sh" 2>&1 || true)"
rm -f "$STUB_BIN/mutt"
printf '%s\n' "$_mail_out" | grep -q 'FAIL.*aerc' \
    && bad "COCKPIT_MAIL=mutt present: aerc absence should not be FAIL" \
           "got: $(printf '%s\n' "$_mail_out" | grep aerc | head -2)" \
    || ok "COCKPIT_MAIL=mutt present: aerc absence is not flagged"

echo
echo "test-bin-manifest.sh: $pass passed, $fail failed"
[ "$fail" -eq 0 ]
