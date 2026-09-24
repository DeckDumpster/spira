#!/usr/bin/env bash
#
# test-bin-manifest.sh — deps.toml parses, and every declared program is fully
# specified (T1: a tier, a purpose, and a version probe).
#
#   ./test-bin-manifest.sh
#
# THE DEFECT THIS SUITE GUARDS AGAINST. A manifest entry with a name but no way to read its
# version cannot be checked for drift — the whole point of shipping a manifest instead of a
# hardcoded list. Before sp-y24c5, SPIRA_BINS carried names and a tier with no version data
# at all: an ancient bd on a box that "has bd" looked identical to a current one.
#
# POSITIVE CONTROL (law-absence-needs-a-positive-control). A scan for missing fields first
# proves it can find one: an undeclared program probed through the same functions must come
# back untiered and unpursed, or the scan is not looking at anything.
#
# covers: spira/conf.sh spira/deps.toml
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

# Positive control: an undeclared program's probe (no matching deps.toml entry) is empty.
_probe_v="$(python3 - "$HERE/deps.toml" "spira-no-such-program" <<'_PY'
import sys, tomllib
with open(sys.argv[1], "rb") as f:
    data = tomllib.load(f)
for d in data.get("dep", []):
    if d["name"] == sys.argv[2]:
        print(d.get("version_probe", ""))
        break
_PY
)"
is "an undeclared program has no version probe" "" "$_probe_v"

# ======================================================================================
echo
echo "a silent downgrade declares what it downgrades to:"
# ======================================================================================
# THE PROPERTY THAT WAS MISSING. bd-embedded is the case that cost a week: absent, it does
# not stop the tests, it moves them onto a shared Dolt server measured at 5-of-6 concurrent
# failures. An operator cannot infer that from "binary not found", so the consequence is
# declared, and testenv/doctor-check.sh and the dev-tools path print it.
_e="ABSENT_bd_embedded"
[ -n "${!_e:-}" ] && ok "bd-embedded declares its absence consequence" \
                  || bad "bd-embedded declares its absence consequence" \
                         "spira_bin_absent bd-embedded is empty — a silent fallback with nothing said about it"

case "${!_e:-}" in
    *"shared Dolt server"*) ok "the consequence names the engine it falls back to" ;;
    *) bad "the consequence names the engine it falls back to" "got [${!_e:-<empty>}]" ;;
esac

echo
echo "test-bin-manifest.sh: $pass passed, $fail failed"
[ "$fail" -eq 0 ]
