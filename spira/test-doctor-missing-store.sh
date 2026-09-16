#!/usr/bin/env bash
#
# test-doctor-missing-store.sh — doctor behaviour when SPIRA_DB has no .beads yet.
#
#   ./test-doctor-missing-store.sh
#
# PROPERTIES UNDER TEST
# ---------------------
# 1. POSITIVE CONTROL. No .beads without SPIRA_DOCTOR_INSTALLING must produce a FAIL
#    so the passing case below proves something real.
# 2. INSTALLING CASE. With SPIRA_DOCTOR_INSTALLING=1 and no .beads, doctor emits WARN
#    (not FAIL), naming phase 3 as the step that will create it.
# 3. REMEDY TEXT. The FAIL does not contain a bare bd init — that command creates an
#    embedded store, which is not the store the harness needs.
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

echo "test-doctor-missing-store.sh"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

BIN="$TMP/bin"
FAKE_DB="$TMP/db"
mkdir -p "$BIN" "$FAKE_DB" "$TMP/run" "$TMP/home"

cat > "$BIN/bd" <<'FAKESCRIPT'
#!/usr/bin/env bash
case "$*" in
    *"list"*) printf '[]\n'; exit 0 ;;
    *)        exit 0 ;;
esac
FAKESCRIPT
chmod +x "$BIN/bd"

cat > "$BIN/systemctl" <<'MOCK'
#!/usr/bin/env bash
exit 0
MOCK
chmod +x "$BIN/systemctl"

touch "$TMP/watchers-empty"

run_doctor() {
    local extra_env=()
    [ "${1:-}" = "--installing" ] && extra_env=(SPIRA_DOCTOR_INSTALLING=1)
    env -i \
        PATH="/usr/local/bin:/usr/bin:/bin" \
        HOME="$TMP/home" \
        SPIRA_CONF=/nonexistent \
        SPIRA_PATH="$BIN" \
        SPIRA_SYSTEMCTL="$BIN/systemctl" \
        SPIRA_DB="$FAKE_DB" \
        SPIRA_RUN="$TMP/run" \
        SPIRA_INSTANCE=prod \
        SPIRA_BD_PIN="$TMP/run/bd-pin" \
        SPIRA_REPO_MAP=/nonexistent \
        SPIRA_NOTIFY=/nonexistent \
        SPIRA_WATCHERS="$TMP/watchers-empty" \
        SPIRA_DOLT_DATA="" \
        "${extra_env[@]+"${extra_env[@]}"}" \
        bash "$HERE/doctor.sh" 2>/dev/null
}

# ==========================================================================
echo
echo "1. POSITIVE CONTROL — missing store without SPIRA_DOCTOR_INSTALLING triggers FAIL:"
# ==========================================================================
ctrl_out="$(run_doctor || true)"
want "positive control: FAIL line present" "FAIL" \
    "$(printf '%s\n' "$ctrl_out" | grep -i 'no .beads\|has no' || true)"

# ==========================================================================
echo
echo "2. INSTALLING CASE — SPIRA_DOCTOR_INSTALLING=1 emits WARN, not FAIL:"
# ==========================================================================
inst_out="$(run_doctor --installing || true)"
want   "installing: WARN references phase 3" "phase 3" "$inst_out"
nowant "installing: no FAIL for missing store" "FAIL" \
    "$(printf '%s\n' "$inst_out" | grep -i 'no .beads\|has no' || true)"

# ==========================================================================
echo
echo "3. REMEDY TEXT — FAIL does not contain a bare bd init:"
# ==========================================================================
nowant "remedy: no bare bd init" "bd init" \
    "$(printf '%s\n' "$ctrl_out" | grep -i 'FAIL\|remedy\|create it' || true)"

echo
printf '  %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
