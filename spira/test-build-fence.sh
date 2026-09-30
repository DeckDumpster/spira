#!/usr/bin/env bash
#
# test-build-fence.sh — build-fence.sh's own positive control.
#
# THE CASE (sp-9uro3, 2026-09-24). sp-upkae certified green under fences-only certification
# (SPIRA_GATE_SUITES=off) and then broke the whole batch's build job: its new dependency
# resolved a crate the pinned toolchain could not parse. Fences-only certification runs no
# suite at all, so nothing on that path ever invoked cargo before the batch's own CI did,
# and the queue had to bisect the batch to find the offending branch.
#
# PROPERTIES
#   T1: change-detection selects a build for a diff that touches Cargo.toml, Cargo.lock, a
#       crate's src/, rust-toolchain, or the Makefile, and skips one that does not — proven
#       with a stubbed `make` so the selection logic is asserted without a real compile.
#   T2: a real, deliberately uncompilable crate makes the fence RED, naming the failure;
#       fixing the same crate (positive control) makes it pass.
#
# tier: T2
# covers: spira/build-fence.sh Makefile Cargo.toml Cargo.lock
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

echo "test-build-fence.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

fence() { bash "$1/spira/build-fence.sh" 2>&1; }

# =========================================================================================
# T1 — CHANGE DETECTION, WITH A STUBBED `make` SO NO COMPILE HAPPENS HERE.
# =========================================================================================
SEL="$TMP/sel"; mkdir -p "$SEL/spira" "$SEL/bin"
cp "$HERE/build-fence.sh" "$SEL/spira/build-fence.sh"
MARK="$TMP/make-called"
cat > "$SEL/bin/make" <<EOF
#!/usr/bin/env bash
printf '%s\n' "\$*" >> "$MARK"
exit 0
EOF
chmod +x "$SEL/bin/make"
sel_fence() {   # sel_fence <files-file>
    rm -f "$MARK"
    SPIRA_GATE_FILES="$1" PATH="$SEL/bin:$PATH" fence "$SEL"
}
made() { [ -f "$MARK" ] && echo yes || echo no; }

echo "1. change-detection selects a build:"
F="$TMP/f1"; printf 'Cargo.lock\n' > "$F"
out="$(sel_fence "$F")"; rc=$?
is "Cargo.lock-only diff: SEEN RED path, build was attempted" "yes" "$(made)"
is "Cargo.lock-only diff: build-fence exits 0 (stub make passes)" "0" "$rc"

F="$TMP/f2"; printf 'spira/some-suite.sh\n' > "$F"
out="$(sel_fence "$F")"; rc=$?
is ".sh-only diff: build is skipped, not attempted" "no" "$(made)"
is ".sh-only diff: build-fence exits 0" "0" "$rc"
want ".sh-only diff: fence says why" "skipped" "$out"

echo "2. STATUS<TAB>FILE form (gate.sh's own shape) is read the same way:"
F="$TMP/f3"; printf 'M\tCargo.toml\n' > "$F"
sel_fence "$F" >/dev/null; is "M<TAB>Cargo.toml selects a build" "yes" "$(made)"

F="$TMP/f4"; printf 'A\trust-toolchain\n' > "$F"
sel_fence "$F" >/dev/null; is "A<TAB>rust-toolchain selects a build" "yes" "$(made)"

F="$TMP/f5"; printf 'M\tbroker/src/main.rs\n' > "$F"
sel_fence "$F" >/dev/null; is "a crate's src/ file selects a build" "yes" "$(made)"

F="$TMP/f6"; printf 'M\tMakefile\n' > "$F"
sel_fence "$F" >/dev/null; is "the Makefile itself selects a build" "yes" "$(made)"

F="$TMP/f7"; printf 'M\tREADME.md\n' > "$F"
sel_fence "$F" >/dev/null; is "an unrelated file with status column is skipped" "no" "$(made)"

echo "2b. no diff context at all is a REFUSAL, not a pass (sp-ufbkh):"
rm -f "$MARK"
out="$(PATH="$SEL/bin:$PATH" fence "$SEL")"; rc=$?
is "no SPIRA_GATE_FILES/SPIRA_GATE_BASE: build is not attempted" "no" "$(made)"
is "no SPIRA_GATE_FILES/SPIRA_GATE_BASE: exits 2, never 0" "2" "$rc"
want "and says it has no diff to check" "no diff to check" "$out"
nowant "and prints no positive control" "fence: build-fence checked" "$out"

echo "2c. the positive control: a checked diff says how much it checked (sp-ufbkh):"
F="$TMP/f9"; printf 'M\tREADME.md\nM\tdocs/x.md\n' > "$F"
out="$(SPIRA_GATE_FILES="$F" PATH="$SEL/bin:$PATH" fence "$SEL" 2>&1)"; rc=$?
is   "a skipped build still exits 0" "0" "$rc"
want "and prints its line with the file count" "fence: build-fence checked 2 changed-files" "$out"
F="$TMP/f10"; : > "$F"
out="$(SPIRA_GATE_FILES="$F" PATH="$SEL/bin:$PATH" fence "$SEL" 2>&1)"; rc=$?
is   "an empty diff is a refusal" "2" "$rc"

echo "3. NEGATIVE CONTROL: a failing stub make is a RED, not swallowed:"
FAIL="$TMP/fail"; mkdir -p "$FAIL/spira" "$FAIL/bin"
cp "$HERE/build-fence.sh" "$FAIL/spira/build-fence.sh"
cat > "$FAIL/bin/make" <<'EOF'
#!/usr/bin/env bash
printf 'error: something is broken\n' >&2
exit 2
EOF
chmod +x "$FAIL/bin/make"
F="$TMP/f8"; printf 'Cargo.lock\n' > "$F"
out="$(SPIRA_GATE_FILES="$F" PATH="$FAIL/bin:$PATH" fence "$FAIL")"; rc=$?
is "a failing build is a RED certification" "1" "$rc"
want "and the fence says so, not a suite" "RED certification" "$out"
want "and it names the command's own error" "something is broken" "$out"

# =========================================================================================
# T2 — A REAL, DELIBERATELY UNCOMPILABLE CRATE. No mocked make or cargo: the real toolchain
# on PATH is the one the production gate's build job uses.
# =========================================================================================
echo "4. a real uncompilable crate goes RED:"
CR="$TMP/crate"; mkdir -p "$CR/spira" "$CR/crate/src"
cp "$HERE/build-fence.sh" "$CR/spira/build-fence.sh"
cat > "$CR/Makefile" <<'EOF'
build:
	cargo build --manifest-path crate/Cargo.toml
EOF
cat > "$CR/crate/Cargo.toml" <<'EOF'
[package]
name = "fixture"
version = "0.1.0"
edition = "2021"
EOF
# SEEN RED FIRST: a syntax error real rustc must refuse (law-a-regression-test-must-be-seen-to-fail).
printf 'fn main() { let x = ; }\n' > "$CR/crate/src/main.rs"
F="$TMP/f9"; printf 'Cargo.lock\n' > "$F"
out="$(SPIRA_GATE_FILES="$F" fence "$CR")"; rc=$?
is "SEEN RED: uncompilable crate fails certification" "1" "$rc"
want "and the cargo error survives into the fence's output" "error" "$out"

echo "5. GREEN AFTER: the same crate, fixed, passes (positive control):"
printf 'fn main() { println!("ok"); }\n' > "$CR/crate/src/main.rs"
out="$(SPIRA_GATE_FILES="$F" fence "$CR")"; rc=$?
is "GREEN AFTER: a compiling crate passes" "0" "$rc"
want "and the fence says so" "make build ok" "$out"

# =========================================================================================
# GATE INTEGRATION. A fence nothing invokes is a file. The repository's gate string names
# build-fence.sh itself (doctor.sh fails a row that does not); the selector (gate-touched.sh,
# now the suite-select binary, sp-wx2tw) stopped calling it (sp-aprxm), because under
# gate_mode=unit the Rust gate drops the step from the string and a second call there would be
# the very cold release build that change removes. The gate crate's own tests
# (`cargo test -p gate`, compose.rs and tests.rs) cover the drop.
# =========================================================================================
echo "6. gate integration:"
# `doctor` is a compiled binary now (sp-yyk47); its gate-compile-check lives in
# doctor/src/lib.rs (this checkout's doctor.sh was retired into the `doctor` crate).
DOCTOR_GATE_CHECK_SRC="$(cd "$HERE/.." && pwd -P)/doctor/src/lib.rs"
if [ -f "$DOCTOR_GATE_CHECK_SRC" ]; then
    dr="$(cat "$DOCTOR_GATE_CHECK_SRC")"
    want "doctor requires the gate string to name build-fence.sh" 'build-fence.sh' "$dr"
else
    echo "  SKIP  doctor/src/lib.rs not found at $DOCTOR_GATE_CHECK_SRC — cannot check the gate-compile-check's own source"
fi
is "build-fence.sh is executable" "0" "$([ -x "$HERE/build-fence.sh" ]; echo $?)"

tl_summary
