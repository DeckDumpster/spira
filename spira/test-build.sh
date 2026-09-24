#!/usr/bin/env bash
#
# test-build.sh — build.sh delegates to make build; tests correct behaviour.
#
# POSITIVE CONTROL FIRST. Before asserting correct behaviour, the test verifies
# that build.sh exists and is executable.
#
# WHAT IS TESTED
# 1. POSITIVE CONTROL: build.sh is present and executable.
# 2. Absent cargo: build.sh exits non-zero (make build fails without cargo).
# 3. Normal run: all five workspace binaries produced at target/release/.
# 4. Idempotence: a second run completes without error.
# 5. --skip-build exits 0 without invoking cargo.
#
# A real cargo is never called. A stub on PATH creates the expected output files.
#
# covers: spira/build.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
REPO_ROOT="$(cd "$HERE/.." && pwd -P)"
pass=0; fail=0
ok()   { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()  { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()   { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want() { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }

echo "test-build.sh"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT
mkdir -p "$TMP/home"

# =========================================================================
echo
echo "POSITIVE CONTROL — build.sh exists and is executable:"
# =========================================================================
if [ -f "$HERE/build.sh" ] && [ -x "$HERE/build.sh" ]; then
    ok "build.sh is present and executable"
else
    bad "build.sh is present and executable" \
        "not found or not executable at $HERE/build.sh"
fi

# =========================================================================
# SHARED FIXTURE
# =========================================================================
HARNESS="$TMP/h"
mkdir -p "$HARNESS"
ln -s "$HERE/build.sh" "$HARNESS/build.sh"
ln -s "$HERE/conf.sh"  "$HARNESS/conf.sh"
printf '# empty\n' > "$HARNESS/repo-map.example"
printf '# empty\n' > "$HARNESS/prefix-map"
printf '# empty\n' > "$HARNESS/watchers"

mkdir -p "$TMP/repo"
# Use the real Makefile from the repo root.
ln -s "$REPO_ROOT/Makefile" "$TMP/repo/Makefile"

# Stub cargo: creates target/release/ binaries relative to CWD.
# When invoked as 'cargo build --release --workspace' from the repo root,
# CWD is $TMP/repo, so binaries land at $TMP/repo/target/release/.
mkdir -p "$TMP/stub"
cat > "$TMP/stub/cargo" << 'ENDSTUB'
#!/usr/bin/env bash
d="$(pwd)/target/release"
mkdir -p "$d"
for b in loom panel broker czar-pass spira-supervise landing-pass; do
    touch "$d/$b"
    chmod +x "$d/$b"
done
ENDSTUB
chmod +x "$TMP/stub/cargo"

# run_build: invoke build.sh in a clean environment with the stub cargo on PATH.
# SPIRA_PATH is prepended to PATH by conf.sh, making the stub cargo visible.
run_build() {
    env -i \
        PATH="/usr/local/bin:/usr/bin:/bin" \
        HOME="$TMP/home" \
        SPIRA_HOME="$HARNESS" \
        SPIRA_REPO="$TMP/repo" \
        SPIRA_COCKPIT="$TMP/cockpit" \
        SPIRA_PATH="$TMP/stub" \
        SPIRA_CONF=/nonexistent \
        SPIRA_DB="$TMP/empty-db" \
        GIT_CONFIG_GLOBAL=/dev/null \
        bash "$HARNESS/build.sh" "$@" 2>&1
}

# run_build_nocargo: no cargo in PATH — exercises the absent-cargo failure path.
run_build_nocargo() {
    env -i \
        PATH="/usr/local/bin:/usr/bin:/bin" \
        HOME="$TMP/home" \
        SPIRA_HOME="$HARNESS" \
        SPIRA_REPO="$TMP/repo" \
        SPIRA_COCKPIT="$TMP/cockpit" \
        SPIRA_CONF=/nonexistent \
        SPIRA_DB="$TMP/empty-db" \
        GIT_CONFIG_GLOBAL=/dev/null \
        bash "$HARNESS/build.sh" "$@" 2>&1
}

# =========================================================================
echo
echo "absent cargo — build.sh exits non-zero:"
# =========================================================================
nocargo_out="$(run_build_nocargo 2>&1)"; nocargo_rc=$?

if [ "$nocargo_rc" -ne 0 ]; then
    ok "absent cargo exits non-zero (exit $nocargo_rc)"
else
    bad "absent cargo exits non-zero" "exited 0 — cargo absence should be fatal"
fi
want "absent cargo mentions Rust install" "Rust" "$nocargo_out"

# =========================================================================
echo
echo "normal run — all six workspace binaries produced:"
# =========================================================================
rm -rf "$TMP/repo/target"

normal_out="$(run_build 2>&1)"; normal_rc=$?

is "normal run exits 0" "0" "$normal_rc"

LOOM_BIN="$TMP/repo/target/release/loom"
PANEL_BIN="$TMP/repo/target/release/panel"
BROKER_BIN="$TMP/repo/target/release/broker"
CZAR_PASS_BIN="$TMP/repo/target/release/czar-pass"
SUPERVISE_BIN="$TMP/repo/target/release/spira-supervise"
LANDING_PASS_BIN="$TMP/repo/target/release/landing-pass"
[ -x "$LOOM_BIN" ]         && ok "loom binary at workspace target/release/" \
    || bad "loom binary at workspace target/release/" "not found at $LOOM_BIN"
[ -x "$PANEL_BIN" ]        && ok "panel binary at workspace target/release/" \
    || bad "panel binary at workspace target/release/" "not found at $PANEL_BIN"
[ -x "$BROKER_BIN" ]       && ok "broker binary at workspace target/release/" \
    || bad "broker binary at workspace target/release/" "not found at $BROKER_BIN"
[ -x "$CZAR_PASS_BIN" ]    && ok "czar-pass binary at workspace target/release/" \
    || bad "czar-pass binary at workspace target/release/" "not found at $CZAR_PASS_BIN"
[ -x "$SUPERVISE_BIN" ]    && ok "spira-supervise binary at workspace target/release/" \
    || bad "spira-supervise binary at workspace target/release/" "not found at $SUPERVISE_BIN"
[ -x "$LANDING_PASS_BIN" ] && ok "landing-pass binary at workspace target/release/" \
    || bad "landing-pass binary at workspace target/release/" "not found at $LANDING_PASS_BIN"

# =========================================================================
echo
echo "idempotence — second run completes without error:"
# =========================================================================
second_out="$(run_build 2>&1)"; second_rc=$?

is "second run exits 0" "0" "$second_rc"

# =========================================================================
echo
echo "--skip-build exits 0 and does not invoke cargo:"
# =========================================================================
cat > "$TMP/stub/cargo" << 'FAILSTUB'
#!/usr/bin/env bash
printf 'cargo: unexpectedly invoked with: %s\n' "$*" >&2
exit 1
FAILSTUB
chmod +x "$TMP/stub/cargo"

skip_out="$(run_build --skip-build 2>&1)"; skip_rc=$?

is "--skip-build exits 0"                "0" "$skip_rc"
want "--skip-build prints loom path"         "loom"            "$skip_out"
want "--skip-build prints panel path"        "panel"           "$skip_out"
want "--skip-build prints broker path"       "broker"          "$skip_out"
want "--skip-build prints czar-pass path"    "czar-pass"       "$skip_out"
want "--skip-build prints supervise path"    "spira-supervise" "$skip_out"
want "--skip-build prints landing-pass path" "landing-pass"    "$skip_out"

# =========================================================================
echo
printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" = 0 ]
