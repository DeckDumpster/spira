#!/usr/bin/env bash
#
# test-install-dolt.sh — install.sh refuses to render dolt units when dolt is absent.
#
#   ./test-install-dolt.sh
#
# PROPERTIES UNDER TEST
# ---------------------
# 1. POSITIVE CONTROL (render): calling the renderer with empty DOLT and a
#    template that contains @DOLT@ must fail before the passing case is trusted.
# 2. RENDER REFUSAL: exits non-zero when DOLT is empty and @DOLT@ is in the
#    template, and names the missing program.
# 3. RENDER CLEAN: exits 0 when DOLT is a valid path.
#
# doctor.sh's own FAIL on an installed unit with an empty ExecStart executable was
# part of the "installed units" section removed by sp-utt1i (build-input/host-preflight
# territory now owned by make and pre-activate, not doctor).
#
# APPROACH FOR RENDER TESTS
# -------------------------
# conf.sh unconditionally adds /usr/local/bin to PATH (the container installs
# dolt there), so PATH manipulation cannot make 'command -v dolt' fail inside
# install.sh. The renderer is instead extracted from install.sh and called
# directly with controlled args, bypassing conf.sh entirely. This tests the
# actual Python code, not a copy: awk reads it from the file at test time.
#
# covers: systemd/install.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
INSTALL_SH="$HERE/../systemd/install.sh"
pass=0; fail=0
ok()      { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()     { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want()    { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant()  { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }
iszero()  { [ "$2" = 0 ] && ok "$1" || bad "$1" "wanted exit 0, got $2"; }
nonzero() { [ "$2" != 0 ] && ok "$1" || bad "$1" "wanted non-zero exit, got 0"; }

echo "test-install-dolt.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

# ---------------------------------------------------------------------------
# Part 1: Python renderer guard when DOLT is empty
#
# The renderer is a Python heredoc inlined inside install.sh's render()
# function. awk extracts it at test time so the test runs the actual code
# rather than a copy. The runner passes controlled argv: template path, then
# the 11 key values (SPIRA_HOME..SPIRA_TESTDB_PORT), then no watcher name.
# ---------------------------------------------------------------------------

RENDERER_PY="$(awk '
    /<<'"'"'PY'"'"'/ { in_py=1; next }
    /^PY$/ && in_py { in_py=0; next }
    in_py { print }
' "$INSTALL_SH")"

if [ -z "$RENDERER_PY" ]; then
    printf 'FATAL: could not extract Python renderer from %s\n' "$INSTALL_SH" >&2
    exit 1
fi

DOLT_TEMPLATE="$HERE/../systemd/dolt-beads.service"

# run_renderer <dolt_value> <template_path>
# Calls the renderer with controlled argv, returns its stdout+stderr and rc.
run_renderer() {
    local dolt_val="$1" template="$2"
    printf '%s\n' "$RENDERER_PY" | \
    python3 - "$template" \
        /fake_home /fake_repo /fake_run /fake_db /fake_cockpit \
        /fake_dolt_data /fake_testdb_data \
        "$dolt_val" \
        /fake_prod prod 3307 \
        2>&1
}

# ==========================================================================
echo
echo "POSITIVE CONTROL — renderer with empty DOLT must fail:"
# ==========================================================================

ctrl_out="$(run_renderer "" "$DOLT_TEMPLATE")"; ctrl_rc=$?
nonzero "positive control: renderer exits non-zero when DOLT is empty" "$ctrl_rc"
want    "positive control: output names dolt"       "dolt"        "$ctrl_out"
want    "positive control: output says not on PATH" "not on PATH" "$ctrl_out"
want    "positive control: install: prefix present" "install:"    "$ctrl_out"
want    "positive control: template file named"     "dolt-beads"  "$ctrl_out"

# ==========================================================================
echo
echo "RENDER REFUSAL — no ExecStart= <space> in stderr/stdout:"
# ==========================================================================
nowant "refusal: no ExecStart= sql-server in output" "ExecStart= sql-server" "$ctrl_out"

# ==========================================================================
echo
echo "RENDER CLEAN — renderer with valid DOLT exits 0:"
# ==========================================================================

# Use a path that exists and is executable so the placeholder resolves cleanly.
DOLT_BIN="$(command -v dolt 2>/dev/null || true)"
if [ -n "$DOLT_BIN" ]; then
    clean_out="$(run_renderer "$DOLT_BIN" "$DOLT_TEMPLATE")"; clean_rc=$?
    iszero  "clean: renderer exits 0 when DOLT is set"       "$clean_rc"
    nowant  "clean: no 'not on PATH' in output"              "not on PATH" "$clean_out"
    want    "clean: ExecStart contains dolt path"            "ExecStart=$DOLT_BIN" "$clean_out"
else
    printf '  skip  clean: dolt not found — cannot verify clean-pass case\n'
fi

# Also verify that a non-dolt template (no @DOLT@) succeeds even with empty DOLT.
NODOLT_TEMPLATE="$HERE/../systemd/spira-sentinel.service"
if [ -f "$NODOLT_TEMPLATE" ]; then
    nodolt_rc=0
    run_renderer "" "$NODOLT_TEMPLATE" >/dev/null 2>&1 || nodolt_rc=$?
    iszero "clean: renderer exits 0 for template without @DOLT@ even when DOLT empty" "$nodolt_rc"
fi

# ==========================================================================
echo
printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" = 0 ]
