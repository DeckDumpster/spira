#!/usr/bin/env bash
#
# test-doctor-dolt.sh — doctor.sh's "the dolt server" section reports correctly when
# SPIRA_DOLT_DATA is set or empty, the data directory exists or does not, and the
# dolt-beads.service is active or not.
#
#   ./test-doctor-dolt.sh
#
# WHAT THIS TESTS
# ---------------
# Three properties are tested:
#
#   1. POSITIVE CONTROL. A known bad case (service not active, dir missing) must first be
#      caught before the passing cases are trusted.
#
#   2. MANAGED CASE. When SPIRA_DOLT_DATA is set and the directory + dolt-beads.service are
#      both present: doctor.sh reports all three ok lines and no FAIL.
#
#   3. UNMANAGED CASE. When SPIRA_DOLT_DATA is empty: doctor.sh reports a single ok line
#      (operator manages it) and no FAIL.
#
#   4. MISSING DIRECTORY. SPIRA_DOLT_DATA is set but the directory does not exist: FAIL.
#
#   5. INACTIVE SERVICE. Directory exists but dolt-beads.service is not active: FAIL.
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

echo "test-doctor-dolt.sh"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

# ---------------------------------------------------------------------------
# Fixture: fake bd, fake systemctl, and a minimal db dir.
# ---------------------------------------------------------------------------
BIN="$TMP/bin"
mkdir -p "$BIN"

# Fake bd handles the three calls doctor.sh makes: list, migrate schema, and seed.
cat > "$BIN/bd" <<'FAKESCRIPT'
#!/usr/bin/env bash
case "$*" in
    *"migrate schema"*) printf '✓ Schema already at v61\n'; exit 0 ;;
    *"list"*"--limit"*) printf '[]\n'; exit 0 ;;
    *) exit 0 ;;
esac
FAKESCRIPT
chmod +x "$BIN/bd"

# ---------------------------------------------------------------------------
# Helper: produce a fake systemctl that answers a given is-active result for
# dolt-beads.service, and active for everything else.
# ---------------------------------------------------------------------------
make_systemctl() {
    local dolt_result="$1"   # "active" or "inactive"
    cat > "$BIN/systemctl" <<MOCK
#!/usr/bin/env bash
case "\$*" in
    *"is-active"*"--quiet"*"dolt-beads"*)
        [ "$dolt_result" = active ] && exit 0 || exit 1 ;;
    *"is-active"*"--quiet"*) exit 0 ;;
    *"list-units"*"active"*"spira-aeon"*) true ;;
    *"list-unit-files"*"spira-watch"*) true ;;
    *"list-units"*"spira-watch"*) true ;;
    *"is-active"*) printf 'active\n' ;;
    *"list-timers"*) true ;;
    *"is-enabled"*) printf 'enabled\n' ;;
esac
exit 0
MOCK
    chmod +x "$BIN/systemctl"
}

# ---------------------------------------------------------------------------
# Helper: set up a minimal environment and run doctor.sh.
# ---------------------------------------------------------------------------
setup_db() {
    mkdir -p "$TMP/db/.beads" "$TMP/run" "$TMP/home"
    # An empty regular file: watchd_rows requires test -f to pass; /dev/null is a char device.
    touch "$TMP/watchers-empty"
}

run_doctor() {
    # env -i strips the caller's environment so an ambient spira.conf cannot
    # interfere. SPIRA_CONF=/nonexistent forces conf.sh to use defaults.
    # SPIRA_DOLT_DATA is passed explicitly per case.
    local extra="${1:-}"
    env -i \
        PATH="/usr/local/bin:/usr/bin:/bin" \
        HOME="$TMP/home" \
        SPIRA_CONF=/nonexistent \
        SPIRA_PATH="$BIN" \
        SPIRA_SYSTEMCTL="$BIN/systemctl" \
        SPIRA_DB="$TMP/db" \
        SPIRA_RUN="$TMP/run" \
        SPIRA_INSTANCE=prod \
        SPIRA_BD_PIN="$TMP/run/bd-pin" \
        SPIRA_REPO_MAP=/nonexistent \
        SPIRA_NOTIFY=/nonexistent \
        SPIRA_WATCHERS="$TMP/watchers-empty" \
        ${extra} \
        bash "$HERE/doctor.sh" 2>/dev/null
}

setup_db

# ==========================================================================
echo
echo "positive control — inactive service is caught before passing cases are trusted:"
# ==========================================================================
DOLT_DIR="$TMP/dolt-data"
mkdir -p "$DOLT_DIR"
printf 'port: 3306\n' > "$DOLT_DIR/dolt-server.yaml"
make_systemctl inactive

ctrl_out="$(run_doctor "SPIRA_DOLT_DATA=$DOLT_DIR" || true)"
want "positive control: FAIL line appears" "FAIL" "$ctrl_out"
want "positive control: service name appears" "dolt-beads" "$ctrl_out"
# The positive control must find the fault; if it doesn't, the test cannot be trusted.

# ==========================================================================
echo
echo "managed case — directory, config, and active service: all ok lines:"
# ==========================================================================
make_systemctl active

managed_out="$(run_doctor "SPIRA_DOLT_DATA=$DOLT_DIR" || true)"
want   "managed: dolt data directory ok" "dolt data directory" "$managed_out"
want   "managed: dolt-server.yaml ok"    "dolt-server.yaml"    "$managed_out"
want   "managed: service active ok"      "dolt-beads.service is active" "$managed_out"
nowant "managed: no FAIL"                "FAIL" "$(printf '%s\n' "$managed_out" | grep 'dolt\|DOLT' || true)"

# ==========================================================================
echo
echo "unmanaged case — SPIRA_DOLT_DATA empty: single ok line, no FAIL:"
# ==========================================================================
unmanaged_out="$(run_doctor "SPIRA_DOLT_DATA=" || true)"
want   "unmanaged: operator-managed ok line" "managed independently" "$unmanaged_out"
nowant "unmanaged: no FAIL for dolt"         "FAIL" "$(printf '%s\n' "$unmanaged_out" | grep 'dolt\|DOLT' || true)"

# ==========================================================================
echo
echo "missing directory — SPIRA_DOLT_DATA set to nonexistent path: FAIL:"
# ==========================================================================
make_systemctl active

missing_out="$(run_doctor "SPIRA_DOLT_DATA=$TMP/does-not-exist" || true)"
want "missing dir: FAIL line" "FAIL" "$missing_out"
want "missing dir: path mentioned" "does-not-exist" "$missing_out"

# ==========================================================================
echo
echo "inactive service — directory exists, yaml exists, service not active: FAIL:"
# ==========================================================================
make_systemctl inactive

inactive_out="$(run_doctor "SPIRA_DOLT_DATA=$DOLT_DIR" || true)"
want "inactive: FAIL line" "FAIL" "$inactive_out"
want "inactive: service name" "dolt-beads.service" "$inactive_out"
nowant "inactive: no ok for service" "ok" "$(printf '%s\n' "$inactive_out" | grep 'dolt-beads.service' || true)"

echo
printf '  %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
