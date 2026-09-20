#!/usr/bin/env bash
#
# test-doctor-cargo.sh — doctor.sh detects a cargo toolchain that is too old to build this
# repo's Cargo.lock (version 4, which requires rustc >= 1.78.0).
#
# POSITIVE CONTROL FIRST. A fake cargo reporting "cargo 1.75.0" must produce a FAIL line
# before the passing cases are trusted. This is the form that satisfies
# law-a-regression-test-must-be-seen-to-fail and law-absence-needs-a-positive-control.
#
# WHAT IS TESTED
# 1. POSITIVE CONTROL: a cargo at 1.75.0 (below the threshold) produces a FAIL line — the
#    defect from sp-tst3, visible before the fix.
# 2. Absent cargo: doctor.sh produces a warn line and no FAIL for cargo.
# 3. Old cargo (1.75.0): doctor.sh produces a FAIL line naming the version.
# 4. Minimum-acceptable cargo (1.78.0): doctor.sh produces an ok line, no FAIL for cargo.
# 5. Current cargo (1.96.0): doctor.sh produces an ok line, no FAIL for cargo.
#
# covers: spira/doctor.sh
# covers: spira/conf.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in output"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in output"; }

echo "test-doctor-cargo.sh"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
mkdir -p "$TMP/home" "$TMP/db/.beads" "$TMP/run"

BIN="$TMP/bin"
mkdir -p "$BIN"

# Fake bd: handles list and migrate schema so doctor.sh does not fail on unrelated sections.
cat > "$BIN/bd" <<'FAKESCRIPT'
#!/usr/bin/env bash
case "$*" in
    *"migrate schema"*) printf '✓ Schema already at v61\n'; exit 0 ;;
    *"list"*"--limit"*) printf '[]\n'; exit 0 ;;
    *) exit 0 ;;
esac
FAKESCRIPT
chmod +x "$BIN/bd"

# Fake systemctl: everything is active; units report disabled so the broker
# enabled-unit FAIL does not fire and obscure the cargo-version check results.
cat > "$BIN/systemctl" <<'FAKESCRIPT'
#!/usr/bin/env bash
case "$*" in
    *"is-active"*"--quiet"*) exit 0 ;;
    *"is-active"*) printf 'active\n' ;;
    *"is-enabled"*) printf 'disabled\n'; exit 1 ;;
    *"list-unit-files"*) true ;;
    *"list-units"*) true ;;
    *"list-timers"*) true ;;
esac
exit 0
FAKESCRIPT
chmod +x "$BIN/systemctl"

# make_cargo VERSION — write a stub cargo to $BIN/cargo that reports the given version.
make_cargo() {
    local ver="$1"
    cat > "$BIN/cargo" <<STUB
#!/usr/bin/env bash
case "\$*" in
    "--version") printf 'cargo %s (abc123 2024-01-01)\n' "$ver"; exit 0 ;;
    *) exit 0 ;;
esac
STUB
    chmod +x "$BIN/cargo"
}

# run_doctor — run doctor.sh in a clean env with BIN prepended.
run_doctor() {
    env -i \
        PATH="/usr/local/bin:/usr/bin:/bin" \
        HOME="$TMP/home" \
        SPIRA_CONF=/nonexistent \
        SPIRA_PATH="$BIN" \
        SPIRA_DB="$TMP/db" \
        SPIRA_RUN="$TMP/run" \
        SPIRA_BD_PIN="$TMP/run/bd-pin" \
        SPIRA_REPO_MAP=/nonexistent \
        SPIRA_NOTIFY=/nonexistent \
        bash "$HERE/doctor.sh" 2>/dev/null
}

# ---------------------------------------------------------------------------
echo
echo "POSITIVE CONTROL — cargo 1.75.0 is caught as too old (fails before the fix):"
# ---------------------------------------------------------------------------
make_cargo 1.75.0
ctrl_out="$(run_doctor || true)"
want "positive control: FAIL line appears"         "FAIL"  "$ctrl_out"
want "positive control: 1.75.0 named in output"    "1.75"  "$ctrl_out"
# If the positive control does not fire, the test cannot be trusted — it would pass
# even against the unfixed code. Record the failure; the later passing cases still run.

# ---------------------------------------------------------------------------
echo
echo "absent cargo — WARN (not FAIL); the loop continues without the panel:"
# ---------------------------------------------------------------------------
rm -f "$BIN/cargo"
absent_out="$(run_doctor || true)"
# A WARN must appear mentioning cargo; no FAIL for cargo specifically.
want   "absent cargo: warn line mentions cargo"  "cargo"  "$absent_out"
nowant "absent cargo: no FAIL for cargo"         "FAIL"   "$(printf '%s\n' "$absent_out" | grep -i 'cargo' || true)"

# ---------------------------------------------------------------------------
echo
echo "old cargo 1.75.0 — FAIL; names the version and the Cargo.lock constraint:"
# ---------------------------------------------------------------------------
make_cargo 1.75.0
old_out="$(run_doctor || true)"
want "old cargo: FAIL line"          "FAIL"    "$old_out"
want "old cargo: version mentioned"  "1.75"    "$old_out"
want "old cargo: threshold mentioned" "1.78"   "$old_out"

# ---------------------------------------------------------------------------
echo
echo "minimum cargo 1.78.0 — OK; at the threshold, not below it:"
# ---------------------------------------------------------------------------
make_cargo 1.78.0
min_out="$(run_doctor || true)"
want   "min cargo: ok line for cargo"  "ok"   "$(printf '%s\n' "$min_out" | grep -i 'cargo' || true)"
nowant "min cargo: no FAIL for cargo"  "FAIL" "$(printf '%s\n' "$min_out" | grep -i 'cargo' || true)"

# ---------------------------------------------------------------------------
echo
echo "current cargo 1.96.0 — OK; well above the threshold:"
# ---------------------------------------------------------------------------
make_cargo 1.96.0
cur_out="$(run_doctor || true)"
want   "current cargo: ok line for cargo"  "ok"   "$(printf '%s\n' "$cur_out" | grep -i 'cargo' || true)"
nowant "current cargo: no FAIL for cargo"  "FAIL" "$(printf '%s\n' "$cur_out" | grep -i 'cargo' || true)"

echo
printf '  %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
