#!/usr/bin/env bash
#
# test-build.sh — build.sh produces both Rust binaries with a stubbed cargo.
#
# POSITIVE CONTROL FIRST. Before asserting correct behaviour, the test verifies that
# build.sh exists and is executable — so a tree without it produces a failure rather
# than vacuous success. That is the form of "fail-first" for a new script: the test that
# would catch its absence comes before any test of what it does.
#
# WHAT IS TESTED
# 1. POSITIVE CONTROL: build.sh is present and executable.
# 2. Absent cargo: build.sh exits 0 and names the feature lost; the harness loop continues.
# 3. Ordering: loom is built before the panel; verified from build.sh's own output messages.
# 4. Binaries exist at the paths doctor.sh checks after a normal run.
# 5. Idempotence: a second run completes without error.
# 6. --skip-build exits 0 without invoking cargo.
#
# A real cargo is never called. A stub on PATH creates the expected output files. Ordering
# is read from build.sh's own "building loom" / "building panel" lines.
#
# covers: spira/build.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
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
# This is the assertion that would fail before the fix, against the original tree.
if [ -f "$HERE/build.sh" ] && [ -x "$HERE/build.sh" ]; then
    ok "build.sh is present and executable"
else
    bad "build.sh is present and executable" \
        "not found or not executable at $HERE/build.sh (positive control: fails before the fix)"
fi

# =========================================================================
# SHARED FIXTURE
# =========================================================================
# Harness dir: holds symlinks to the real build.sh and conf.sh. SPIRA_HOME will be set
# to this directory so conf.sh resolves SPIRA_REPO and SPIRA_COCKPIT from our overrides.
HARNESS="$TMP/h"
mkdir -p "$HARNESS"
ln -s "$HERE/build.sh" "$HARNESS/build.sh"
ln -s "$HERE/conf.sh"  "$HARNESS/conf.sh"
# Minimal stubs so conf.sh resolves cleanly without complaining about missing files:
printf '# empty\n' > "$HARNESS/repo-map.example"
printf '# empty\n' > "$HARNESS/prefix-map"
printf '# empty\n' > "$HARNESS/watchers"

# Source directories build.sh will cd into.
LOOM_SRC="$TMP/repo/loom"
PANEL_SRC="$TMP/cockpit/panel"
BROKER_SRC="$TMP/repo/broker"
mkdir -p "$LOOM_SRC" "$PANEL_SRC" "$BROKER_SRC"

# Stub cargo: creates target/release/<dirname> relative to the caller's working directory,
# which is what cargo build --release produces. The binary name comes from the directory
# name (loom → loom/target/release/loom; panel → panel/target/release/panel).
mkdir -p "$TMP/stub"
cat > "$TMP/stub/cargo" << ENDSTUB
#!/usr/bin/env bash
d="\$(pwd)/target/release"
mkdir -p "\$d"
bin="\$(basename "\$(pwd)")"
touch "\$d/\$bin"
chmod +x "\$d/\$bin"
ENDSTUB
chmod +x "$TMP/stub/cargo"

# run_build: invoke build.sh in a clean, controlled environment with the stub cargo on PATH.
# SPIRA_REPO and SPIRA_COCKPIT are overridden so conf.sh derives SPIRA_LOOM_BIN and
# SPIRA_PANEL inside our temp tree instead of the operator's real paths.
# SPIRA_PATH is used (not the raw PATH) because conf.sh reassembles PATH as:
#   "${SPIRA_PATH:+$SPIRA_PATH:}$HOME/.local/bin:/usr/local/bin:/usr/bin:/bin"
# — so a stub placed only in the initial PATH would be dropped after conf.sh runs.
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

# run_build_nocargo: same as run_build but with no cargo in SPIRA_PATH, to exercise the
# absent-cargo path. SPIRA_PATH is left empty so no stub is prepended.
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
echo "absent cargo — build.sh exits 0 and names the feature lost:"
# =========================================================================
nocargo_out="$(run_build_nocargo 2>&1)"; nocargo_rc=$?

is "absent cargo exits 0" "0" "$nocargo_rc"
want "absent cargo names the feature lost" "features lost" "$nocargo_out"
want "absent cargo says the loop continues" "loop" "$nocargo_out"

# =========================================================================
echo
echo "normal run — all three binaries produced, loom before panel before broker:"
# =========================================================================
rm -rf "$TMP/repo/loom/target" "$TMP/cockpit/panel/target" "$TMP/repo/broker/target"

normal_out="$(run_build 2>&1)"; normal_rc=$?

is "normal run exits 0" "0" "$normal_rc"

# Ordering: loom before panel before broker.
loom_line="$(printf '%s\n' "$normal_out" | grep -n 'building loom' | head -1 | cut -d: -f1)"
panel_line="$(printf '%s\n' "$normal_out" | grep -n 'building panel' | head -1 | cut -d: -f1)"
broker_line="$(printf '%s\n' "$normal_out" | grep -n 'building broker' | head -1 | cut -d: -f1)"
if [ -n "${loom_line:-}" ] && [ -n "${panel_line:-}" ] && \
   [ "$loom_line" -lt "$panel_line" ] 2>/dev/null; then
    ok "loom announced before panel (line $loom_line vs $panel_line)"
else
    bad "loom announced before panel" \
        "loom_line='$loom_line' panel_line='$panel_line' in output: $normal_out"
fi
if [ -n "${panel_line:-}" ] && [ -n "${broker_line:-}" ] && \
   [ "$panel_line" -lt "$broker_line" ] 2>/dev/null; then
    ok "panel announced before broker (line $panel_line vs $broker_line)"
else
    bad "panel announced before broker" \
        "panel_line='$panel_line' broker_line='$broker_line' in output: $normal_out"
fi

# All three binaries exist at the paths build.sh derives from SPIRA_REPO and SPIRA_COCKPIT.
LOOM_BIN="$TMP/repo/loom/target/release/loom"
PANEL_BIN="$TMP/cockpit/panel/target/release/panel"
BROKER_BIN="$TMP/repo/broker/target/release/broker"
[ -x "$LOOM_BIN" ]   && ok "loom binary exists at SPIRA_LOOM_BIN path" \
    || bad "loom binary exists at SPIRA_LOOM_BIN path" "not found or not executable at $LOOM_BIN"
[ -x "$PANEL_BIN" ]  && ok "panel binary exists at SPIRA_PANEL path" \
    || bad "panel binary exists at SPIRA_PANEL path" "not found or not executable at $PANEL_BIN"
[ -x "$BROKER_BIN" ] && ok "broker binary exists at SPIRA_BROKER_BIN path" \
    || bad "broker binary exists at SPIRA_BROKER_BIN path" "not found or not executable at $BROKER_BIN"

# =========================================================================
echo
echo "idempotence — second run completes without error:"
# =========================================================================
second_out="$(run_build 2>&1)"; second_rc=$?

is "second run exits 0" "0" "$second_rc"
want "second run mentions loom"   "loom"   "$second_out"
want "second run mentions panel"  "panel"  "$second_out"
want "second run mentions broker" "broker" "$second_out"

# =========================================================================
echo
echo "--skip-build exits 0 and does not invoke cargo:"
# =========================================================================
# Replace stub cargo with one that fails loudly on any call.
cat > "$TMP/stub/cargo" << 'FAILSTUB'
#!/usr/bin/env bash
printf 'cargo: unexpectedly invoked with: %s\n' "$*" >&2
exit 1
FAILSTUB
chmod +x "$TMP/stub/cargo"

skip_out="$(run_build --skip-build 2>&1)"; skip_rc=$?

is "--skip-build exits 0"            "0" "$skip_rc"
want "--skip-build prints loom path"   "loom"   "$skip_out"
want "--skip-build prints panel path"  "panel"  "$skip_out"
want "--skip-build prints broker path" "broker" "$skip_out"

# =========================================================================
echo
printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" = 0 ]
