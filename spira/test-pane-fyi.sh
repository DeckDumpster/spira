#!/usr/bin/env bash
#
# test-pane-fyi.sh — FYI framing and insight lifecycle, targeted Rust checks.
#
#   ./test-pane-fyi.sh
#
# WHAT IT DOES. Runs the panel's Rust tests that verify the two properties
# sp-pane-insights-fyi required, by name. test-panel.sh runs the full cargo
# test suite on a schedule; this suite names the subset so "which tests prove
# the FYI view is correct?" has a direct answer in the suite list.
#
# The two properties:
#
#   FRAMING — insights carry no decide-shaped affordance:
#     fyi                  all tests whose name contains "fyi" (tab label,
#                          footer, reader footer, turn marker, empty state,
#                          dismissed tab rename)
#     why_it_matters       rule beneath the thread names the body "why it matters"
#     insight              all tests whose name contains "insight" (body cleanup,
#                          lifecycle, undismissed visibility)
#     enacted              promoted insights carry § and cite their statute
#     statute              the compose row says "statute" over an enact prompt
#
#   LIFECYCLE — a dismissed insight leaves the pane and stays retrievable:
#     dismiss              all dismiss/restore/archived tests
#     promoted             a promoted row is not a dismissed one
#
# POSITIVE CONTROL: running an empty filter would silently pass with 0 tests.
# The suite fails if cargo reports fewer than 15 tests run — the minimum count
# of the named subset. This catches a stale filter or a renamed test before the
# timed run lets it pass quietly.
#
# WHERE IT RUNS: the TIMED set, like test-panel.sh. They are complementary:
# test-panel.sh proves nothing broke; this suite proves the specific properties
# named in the bead's acceptance criteria.
#
# defect: sp-pane-insights-fyi
# covers: cockpit/panel/src/render.rs cockpit/panel/src/model.rs cockpit/panel/src/store.rs
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
PANEL_ROOT="$HERE/../cockpit/panel"

pass=0; fail=0
ok()  { printf '  ok    %s\n' "$1"; pass=$((pass+1)); }
bad() { printf '  FAIL  %s\n        %s\n' "$1" "${2:-}"; fail=$((fail+1)); }

# Resolve cargo before anything can overwrite PATH.
CARGO_BIN="$(command -v cargo 2>/dev/null || true)"
if [ -z "$CARGO_BIN" ] && [ -x "$HOME/.cargo/bin/cargo" ]; then
    CARGO_BIN="$HOME/.cargo/bin/cargo"
fi
if [ -z "$CARGO_BIN" ]; then
    echo "SKIP test-pane-fyi: cargo not found — install Rust at https://rustup.rs/" >&2
    exit 77
fi
# RUSTC MUST BE ON PATH TOO, NOT JUST CARGO. cargo execs `rustc` BY NAME, so capturing
# cargo's absolute path is only half the job: conf.sh replaces PATH wholesale with the
# harness's own tool directories, and the toolchain directory is not among them. In a
# container that left cargo resolvable and rustc not, and the suite died with
#   error: could not execute process `rustc -vV` (never executed)
# which reads as a broken crate rather than a broken PATH.
#
# SPIRA_PATH AND NOT JUST PATH. conf.sh is sourced BELOW (testdb.sh pulls it in), and it
# rebuilds PATH from SPIRA_PATH plus a fixed list — so assigning PATH here alone is undone a
# few lines later, which is how the first attempt at this fix changed nothing. SPIRA_PATH is
# the seam conf.sh honours, and testdb.sh already uses it for exactly this purpose when it
# puts the fixture's bd on PATH. Both are set so the order of what follows cannot matter.
_CARGO_DIR="$(dirname "$CARGO_BIN")"
export SPIRA_PATH="$_CARGO_DIR${SPIRA_PATH:+:$SPIRA_PATH}"
export PATH="$_CARGO_DIR:$PATH"

if [ ! -d "$PANEL_ROOT" ]; then
    echo "SKIP test-pane-fyi: panel directory not found at $PANEL_ROOT" >&2
    exit 77
fi

echo "panel FYI / insight tests — sp-pane-insights-fyi acceptance criteria"

# Run the named subset. Each filter is a substring matched against the full
# test path (module::tests::name), so "fyi" picks up every *fyi* test name,
# "insight" picks up every *insight* test name, and so on. Multiple filters
# run separately because cargo --test-opts takes one filter at a time.
FILTERS=(fyi why_it_matters insight enacted statute dismiss promoted)

run_filter() {
    "$CARGO_BIN" test --manifest-path "$PANEL_ROOT/Cargo.toml" \
        --no-fail-fast -- "$1" 2>&1
}

total_run=0
for filter in "${FILTERS[@]}"; do
    out="$(run_filter "$filter")"
    status=$?

    # Count the tests that ran under this filter.
    ran="$(grep -c '^test .* \.\.\. ok$\|^test .* \.\.\. FAILED$' <<< "$out" || true)"
    total_run=$((total_run + ran))

    while IFS= read -r line; do
        case "$line" in
            "test "*" ... ok")
                name="${line#test }"; name="${name% ... ok}"
                ok "$name" ;;
            "test "*" ... FAILED")
                name="${line#test }"; name="${name% ... FAILED}"
                bad "$name" ;;
            "FAILED"*|*"error["*)
                printf '  %s\n' "$line" ;;
        esac
    done <<< "$out"

    if [ $status -ne 0 ] && ! grep -q "^test .* \.\.\. FAILED$" <<< "$out"; then
        bad "cargo test --$filter" "exit $status — compile error or runner failure"
        printf '%s\n' "$out"
    fi
done

echo
# POSITIVE CONTROL: the named subset must produce at least 15 tests. A renamed
# test or a stale filter returns 0 here instead of failing loudly.
if [ "$total_run" -lt 15 ]; then
    bad "filter coverage" "$total_run tests matched — expected ≥15; check that the filters still match test names"
else
    ok "filter coverage: $total_run tests matched"
fi

printf 'test-pane-fyi: %d ok, %d fail\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
