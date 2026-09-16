#!/usr/bin/env bash
#
# test-doctor-bd-tag.sh — doctor.sh fails when the installed bd version does not match
# the configured SPIRA_BD_TAG pin, and passes when it does.
#
# WHAT THIS TESTS
# ---------------
# The pin BD_TAG_PIN in build-bd.sh governed what was BUILT but never compared against
# the binary already RUNNING. A box could run a version the build script would have
# refused, indefinitely, with no check saying so. SPIRA_BD_TAG in conf.sh exposes the
# intended version as configuration that both the builder and doctor.sh read.
#
# Four properties are tested:
#
#   1. POSITIVE CONTROL. A fake bd that reports a wrong version is itself caught before
#      the main assertions run. Without this a check that never fires reads identically
#      to one that fires and passes (law-absence-needs-a-positive-control).
#
#   2. MISMATCH BEHIND. When bd version is below the pin, doctor.sh exits non-zero
#      and names both the installed version and the pin.
#
#   3. MISMATCH AHEAD. When bd version is above the pin, doctor.sh exits non-zero and
#      distinguishes "AHEAD" — the failure class where a schema migration is one-way.
#
#   4. MATCH. When bd version matches the pin exactly, doctor.sh passes the check.
#
# covers: spira/doctor.sh spira/conf.sh spira/build-bd.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()    { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()   { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want()  { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in output"; }
nowant(){ [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in output"; }

echo "test-doctor-bd-tag.sh"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

# A fake-bd factory: $1=version-string reported by `bd version`, $2=migrate-schema output.
# Produces a binary at $TMP/bin/bd that answers the two calls doctor.sh makes.
make_fake_bd() {
    local ver="$1" schema_out="${2:-✓ Schema already at v65}"
    mkdir -p "$TMP/bin"
    cat > "$TMP/bin/bd" <<FAKESCRIPT
#!/usr/bin/env bash
case "\$*" in
    *"version"*)        printf 'bd version %s (abc1234)\n' "$ver"; exit 0 ;;
    *"migrate schema"*) printf '%s\n' "$schema_out"; exit 0 ;;
    *"list"*"--limit"*) printf '[]\n'; exit 0 ;;
    *"show"*)           printf '{}\n'; exit 0 ;;
    *) exit 0 ;;
esac
FAKESCRIPT
    chmod +x "$TMP/bin/bd"
}

setup_env() {
    mkdir -p "$TMP/db/.beads" "$TMP/run"
}

# Run doctor.sh in an isolated environment. SPIRA_BD_TAG sets the expected tag;
# SPIRA_BD points at the fake binary under test.
run_doctor() {
    local bd_tag="${1:-v1.2.1}" extra_env="${2:-}"
    env -i \
        PATH="/usr/local/bin:/usr/bin:/bin" \
        HOME="$TMP/home" \
        SPIRA_CONF=/nonexistent \
        SPIRA_PATH="$TMP/bin" \
        SPIRA_BD="$TMP/bin/bd" \
        SPIRA_DB="$TMP/db" \
        SPIRA_RUN="$TMP/run" \
        SPIRA_BD_PIN="$TMP/run/bd-pin" \
        SPIRA_BD_TAG="$bd_tag" \
        SPIRA_REPO_MAP=/nonexistent \
        SPIRA_NOTIFY=/nonexistent \
        ${extra_env} \
        bash "$HERE/doctor.sh" 2>/dev/null
}

setup_env

# ==========================================================================
echo
echo "positive control — fake bd reporting a wrong version is detectable:"
# ==========================================================================
# A fake bd that reports 1.2.0 while the pin is v1.2.1 must produce a FAIL.
# If the check never fires, it would report OK — catching that silence is the
# point of the positive control.
make_fake_bd "1.2.0"
ctrl_out="$(run_doctor "v1.2.1" || true)"

want "positive control: 'v1.2.0' appears in output" "v1.2.0" "$ctrl_out"
want "positive control: 'v1.2.1' appears in output" "v1.2.1" "$ctrl_out"
want "positive control: FAIL line appears"           "FAIL"    "$ctrl_out"

# ==========================================================================
echo
echo "mismatch BEHIND — installed version is older than the pin:"
# ==========================================================================
make_fake_bd "1.2.0"
run_doctor "v1.2.1" > "$TMP/behind.out" 2>/dev/null && behind_rc=0 || behind_rc=$?
behind_out="$(cat "$TMP/behind.out")"

want   "behind: FAIL names installed version"   "v1.2.0"  "$behind_out"
want   "behind: FAIL names the pin"             "v1.2.1"  "$behind_out"
want   "behind: FAIL says BEHIND"               "BEHIND"  "$behind_out"
nowant "behind: does not say AHEAD"             "AHEAD"   "$behind_out"
if [ "${behind_rc:-0}" -ne 0 ]; then
    ok "behind: doctor.sh exits non-zero (rc=$behind_rc)"
else
    bad "behind: doctor.sh exits non-zero" "exited 0"
fi

# ==========================================================================
echo
echo "mismatch AHEAD — installed version is newer than the pin:"
# ==========================================================================
make_fake_bd "1.2.2"
run_doctor "v1.2.1" > "$TMP/ahead.out" 2>/dev/null && ahead_rc=0 || ahead_rc=$?
ahead_out="$(cat "$TMP/ahead.out")"

want   "ahead: FAIL names installed version"    "v1.2.2"  "$ahead_out"
want   "ahead: FAIL names the pin"              "v1.2.1"  "$ahead_out"
want   "ahead: FAIL says AHEAD"                 "AHEAD"   "$ahead_out"
nowant "ahead: does not say BEHIND"             "BEHIND"  "$ahead_out"
if [ "${ahead_rc:-0}" -ne 0 ]; then
    ok "ahead: doctor.sh exits non-zero (rc=$ahead_rc)"
else
    bad "ahead: doctor.sh exits non-zero" "exited 0"
fi

# ==========================================================================
echo
echo "match — installed version equals the pin:"
# ==========================================================================
make_fake_bd "1.2.1"
match_out="$(run_doctor "v1.2.1" || true)"

want   "match: ok line mentions tag pin"        "tag pin"  "$match_out"
want   "match: ok line names the pin"           "v1.2.1"   "$match_out"
nowant "match: no FAIL for version"             "version mismatch" \
       "$(printf '%s\n' "$match_out" | grep 'FAIL' || true)"

echo
printf '  %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
