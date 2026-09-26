# plan-bin.sh — resolve_test_plan_bin, shared by plan-lint.sh and plan-matrix.sh so
# there is one place that knows how to find or build the test-plan binary.
# Sourced, never executed; requires ROOT to already be set by the caller.

# resolve_test_plan_bin -> prints the path to a built test-plan binary, or returns 1 having
# printed why not. SPIRA_TEST_PLAN_BIN (conf.sh) is preferred so an activated release with no
# cargo on PATH still works; a source checkout builds it on demand, the same fallback
# test-spira-config.sh uses for cargo itself.
resolve_test_plan_bin() {
    if [ -n "${SPIRA_TEST_PLAN_BIN:-}" ] && [ -x "$SPIRA_TEST_PLAN_BIN" ]; then
        printf '%s\n' "$SPIRA_TEST_PLAN_BIN"
        return 0
    fi
    local cargo_bin
    cargo_bin="$(command -v cargo 2>/dev/null || true)"
    if [ -z "$cargo_bin" ] && [ -x "$HOME/.cargo/bin/cargo" ]; then
        cargo_bin="$HOME/.cargo/bin/cargo"
    fi
    if [ -z "$cargo_bin" ]; then
        printf 'plan-bin: cargo not found on PATH or at ~/.cargo/bin — cannot build test-plan\n' >&2
        return 1
    fi
    if ! PATH="$(dirname "$cargo_bin"):$PATH" "$cargo_bin" build --release \
        --manifest-path "$ROOT/test-plan/Cargo.toml" >&2
    then
        printf 'plan-bin: building test-plan failed\n' >&2
        return 1
    fi
    # CARGO_TARGET_DIR, when set (the testenv container points it off the bind mount — see
    # testenv.sh's own note — so the build above never wrote under $ROOT at all), is where
    # cargo actually put the binary; only its absence means cargo used $ROOT/target.
    printf '%s\n' "${CARGO_TARGET_DIR:-$ROOT/target}/release/test-plan"
}
