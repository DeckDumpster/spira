#!/usr/bin/env bash
# test-landing-build.sh — land-build-ensure.sh fires build.sh when a cargo-built
# binary is absent with its unit enabled; skips it when the binary is present.
#
# POSITIVE CONTROL FIRST. Each silent assertion is preceded by a case that proves
# the trigger fires before trusting that it is quiet (law-absence-needs-a-positive-control).
#
# WHAT IS TESTED
# 1. POSITIVE CONTROL: land-build-ensure.sh is present and executable.
# 2. Binary absent + unit enabled + cargo stubbed → build.sh is invoked.
# 3. Binary present + no source change → build.sh is NOT invoked.
# 4. cargo absent → build.sh is NOT invoked even when binary is absent.
#
# covers: spira/land-build-ensure.sh spira/landing.sh spira/build.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }

echo "test-landing-build.sh"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

# =========================================================================
echo
echo "POSITIVE CONTROL — land-build-ensure.sh exists and is executable:"
# =========================================================================
if [ -f "$HERE/land-build-ensure.sh" ] && [ -x "$HERE/land-build-ensure.sh" ]; then
    ok "land-build-ensure.sh is present and executable"
else
    bad "land-build-ensure.sh is present and executable" \
        "not found or not executable (positive control: fails before the fix)"
fi

# =========================================================================
# SHARED FIXTURE
# =========================================================================
# A minimal git repo with no reflog (so _be_src_changed stays 0) lets us test
# the absent-binary trigger in isolation.
REPO="$TMP/repo"
mkdir -p "$REPO/spira"
git -C "$REPO" init -q
git -C "$REPO" config user.email t@t
git -C "$REPO" config user.name t
touch "$REPO/spira/.keep"
git -C "$REPO" add .
git -C "$REPO" commit -q -m "init"
# No second commit → no @{1} reflog entry → source-changed check is always 0.

# Stub build.sh: records that it was called in a marker file.
BUILD_MARKER="$TMP/build_called"
cat > "$REPO/spira/build.sh" <<BSTUB
#!/usr/bin/env bash
touch "$BUILD_MARKER"
printf 'build.sh: stub invoked\n'
BSTUB
chmod +x "$REPO/spira/build.sh"

# Stub cargo: land-build-ensure.sh checks 'command -v cargo' before doing anything.
mkdir -p "$TMP/stub"
cat > "$TMP/stub/cargo" <<'CSTUB'
#!/usr/bin/env bash
exit 0
CSTUB
chmod +x "$TMP/stub/cargo"

# Stub systemctl: by default reports every unit as enabled.
SC_LOG="$TMP/sc.log"
cat > "$TMP/stub/systemctl" <<SCMOCK
#!/usr/bin/env bash
printf '%s\n' "\$*" >> "$SC_LOG"
case "\$*" in
    *"is-enabled"*) printf 'enabled\n'; exit 0 ;;
    *) exit 0 ;;
esac
SCMOCK
chmod +x "$TMP/stub/systemctl"

# run_ensure: invoke land-build-ensure.sh in a controlled environment.
# SPIRA_BROKER_BIN is the one binary we manipulate in the tests.
FAKE_BROKER_BIN="$TMP/broker-bin"
run_ensure() {
    env -i \
        PATH="$TMP/stub:/usr/local/bin:/usr/bin:/bin" \
        HOME="$TMP/home" \
        SPIRA_REPO="$REPO" \
        SPIRA_LOOM_BIN="$FAKE_BROKER_BIN" \
        SPIRA_BROKER_BIN="$FAKE_BROKER_BIN" \
        SPIRA_PANEL="$FAKE_BROKER_BIN" \
        SPIRA_CZAR_PASS_BIN="$FAKE_BROKER_BIN" \
        SPIRA_INSTANCE=prod \
        SPIRA_SYSTEMCTL="$TMP/stub/systemctl" \
        bash "$HERE/land-build-ensure.sh" 2>&1
}

# run_ensure_nocargo: same without cargo on PATH.
run_ensure_nocargo() {
    env -i \
        PATH="/usr/local/bin:/usr/bin:/bin" \
        HOME="$TMP/home" \
        SPIRA_REPO="$REPO" \
        SPIRA_LOOM_BIN="$FAKE_BROKER_BIN" \
        SPIRA_BROKER_BIN="$FAKE_BROKER_BIN" \
        SPIRA_PANEL="$FAKE_BROKER_BIN" \
        SPIRA_CZAR_PASS_BIN="$FAKE_BROKER_BIN" \
        SPIRA_INSTANCE=prod \
        SPIRA_SYSTEMCTL="$TMP/stub/systemctl" \
        bash "$HERE/land-build-ensure.sh" 2>&1
}

# =========================================================================
echo
echo "binary absent + unit enabled + cargo stubbed → build.sh is invoked:"
# =========================================================================
# FAKE_BROKER_BIN does not exist → absent binary.
# Stub systemctl reports every unit as enabled.
rm -f "$BUILD_MARKER" "$SC_LOG"

absent_out="$(run_ensure 2>&1)"; absent_rc=$?

is "absent binary + enabled unit exits 0" "0" "$absent_rc"
want "absent binary + enabled unit triggers build" "cargo binary absent" "$absent_out"
want "absent binary + enabled unit runs build.sh" "build.sh: stub invoked" "$absent_out"
[ -f "$BUILD_MARKER" ] \
    && ok  "build.sh marker created (build.sh was called)" \
    || bad "build.sh marker created (build.sh was called)" \
           "marker file absent — build.sh was not invoked"

# =========================================================================
echo
echo "binary present + no source change → build.sh is NOT invoked:"
# =========================================================================
# Create the fake binary so it is executable → land-build-ensure.sh should skip.
touch "$FAKE_BROKER_BIN"; chmod +x "$FAKE_BROKER_BIN"
rm -f "$BUILD_MARKER" "$SC_LOG"

present_out="$(run_ensure 2>&1)"; present_rc=$?

is "binary present exits 0" "0" "$present_rc"
nowant "binary present does not trigger build" "running build.sh" "$present_out"
[ ! -f "$BUILD_MARKER" ] \
    && ok  "build.sh marker absent (build.sh was not called)" \
    || bad "build.sh marker absent (build.sh was not called)" \
           "marker file present — build.sh was invoked when it should not have been"

# =========================================================================
echo
echo "cargo absent → build.sh is NOT invoked even when binary is absent:"
# =========================================================================
rm -f "$FAKE_BROKER_BIN" "$BUILD_MARKER" "$SC_LOG"

nocargo_out="$(run_ensure_nocargo 2>&1)"; nocargo_rc=$?

is "no cargo exits 0" "0" "$nocargo_rc"
nowant "no cargo does not trigger build" "running build.sh" "$nocargo_out"
[ ! -f "$BUILD_MARKER" ] \
    && ok  "no cargo: build.sh marker absent" \
    || bad "no cargo: build.sh marker absent" \
           "marker present — build.sh invoked without cargo"

# =========================================================================
echo
printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" = 0 ]
