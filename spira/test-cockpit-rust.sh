#!/usr/bin/env bash
#
# test-cockpit-rust.sh — the native cargo job for cockpit-observability's Rust crates.
#
#   ./test-cockpit-rust.sh
#
# THE FAILURE THIS SUITE EXISTS FOR. test-panel.sh and the deleted test-loom.sh each ran
# `cargo test` and reported it as ONE ok/FAIL line — 120 tests in the panel collapsed to a
# suite that claimed 97 in its own header and never noticed the drift. A red said nothing
# about which Rust test broke; a reader had to open the log and read cargo's own output by
# eye.
#
# WHAT THIS DOES INSTEAD. `cargo test`'s stdout already names every test as it finishes
# (`test <path> ... ok` / `... FAILED`); this suite parses those lines and calls testlib.sh's
# ok/bad once per Rust test, so each one gets its own TAP14 line and JSONL row keyed to the
# UC ids on the # covers: line below, instead of being folded into a single suite-level count.
#
# ONE JOB, NOT TWO SHIMS. Loom's tests/endpoint.rs hits a real `bd` on a throwaway database
# and refuses to run without LOOM_TEST_DB/LOOM_TEST_BD set (see that file's own comment) —
# that fixture-building was the deleted test-loom.sh's whole job. It is folded in here rather
# than kept as a second file so the panel and loom tests build against one cached workspace
# target (`cargo test -p panel -p loom` in a single invocation) instead of two separate
# `cargo test` processes each paying their own link time.
#
# TRADE-OFF: the panel's tests do not themselves need a database, but they SKIP along with
# loom's when no fixture is reachable, because they now share one invocation. Given the
# embedded fixture this harness normally has on hand, that is cheaper than two suites that
# each resolve cargo and PATH independently.
#
# tier: T2
# covers: cockpit/panel/src/* loom/src/* loom/tests/* UC-cockpit-observability-27 UC-cockpit-observability-28 UC-cockpit-observability-29 UC-cockpit-observability-31 UC-cockpit-observability-32
# timeout: 180
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
ROOT="$(cd "$HERE/.." && pwd -P)"
. "$HERE/testlib.sh"

# RESOLVE THE TOOLCHAIN EXPLICITLY, AND SKIP RATHER THAN FAIL WHEN IT IS ABSENT. cargo execs
# `rustc` BY NAME, and conf.sh (sourced by testdb.sh below) overwrites PATH with the harness's
# own tool directories, which do not include ~/.cargo/bin — see test-panel.sh's own note.
# SPIRA_PATH is the seam conf.sh honours for exactly this, so both are set.
CARGO_BIN="$(command -v cargo 2>/dev/null || true)"
if [ -z "$CARGO_BIN" ] && [ -x "$HOME/.cargo/bin/cargo" ]; then
    CARGO_BIN="$HOME/.cargo/bin/cargo"
fi
[ -n "$CARGO_BIN" ] || skip "cargo not found on PATH or at ~/.cargo/bin — install Rust: https://rustup.rs/"
_CARGO_DIR="$(dirname "$CARGO_BIN")"
export SPIRA_PATH="$_CARGO_DIR${SPIRA_PATH:+:$SPIRA_PATH}"
export PATH="$_CARGO_DIR:$PATH"

# shellcheck source=/dev/null
. "$HERE/testdb.sh"
testdb_available || skip "no fixture database reachable — loom's endpoint tests did not run"

TMP="$(mktemp -d)"; trap 'testdb_drop >/dev/null 2>&1; rm -rf "$TMP"' EXIT INT TERM
testdb_up cockpit-rust >/dev/null 2>&1 || bail "testdb_up failed — cannot seed the loom fixture"

# The same four-bead, three-edge fixture the deleted test-loom.sh built: two open, one
# in-progress, one closed. count=3, edges=2, dropped_edges=1 — three non-zero expectations,
# so a failure here is distinguishable from "nothing was there" (loom/tests/endpoint.rs).
bdq() { bd -C "$SPIRA_DB" "$@"; }
bdq create "open bead"      -t task -p 1 -l repo:alpha      --id sp-aaa >/dev/null 2>&1
bdq create 'beta "quoted" and a \ backslash' \
           -t epic -p 1 -l repo:alpha      --id sp-bbb >/dev/null 2>&1
bdq create "in-progress bead" -t task -p 2                  --id sp-ccc >/dev/null 2>&1
bdq create "closed bead"    -t epic -p 3 -l repo:alpha      --id sp-zzz >/dev/null 2>&1
bdq update sp-ccc --status in_progress >/dev/null 2>&1
bdq update sp-zzz --status closed >/dev/null 2>&1
bdq update sp-ccc --parent sp-bbb >/dev/null 2>&1          # parent-child edge
bdq dep add sp-aaa sp-ccc >/dev/null 2>&1                  # sp-aaa blocks sp-ccc
bdq dep add sp-bbb sp-zzz >/dev/null 2>&1                  # sp-bbb blocks sp-zzz (closed — dropped)

export LOOM_TEST_DB="$SPIRA_DB"
export LOOM_TEST_BD="${SPIRA_BD:-bd}"

# _report_cargo_tests <cargo-output-file> — call ok/bad once per `test <name> ... ok|FAILED`
# line cargo printed. cargo interleaves output from parallel test threads, but each test's
# own result line is whole and unique, so line-based parsing needs no --test-threads=1.
_report_cargo_tests() {
    local out="$1" line name detail
    while IFS= read -r line; do
        case "$line" in
            "test "*" ... ok")
                name="${line#test }"; name="${name% ... ok}"
                ok "$name"
                ;;
            "test "*" ... FAILED")
                name="${line#test }"; name="${name% ... FAILED}"
                detail="$(awk -v t="---- $name stdout ----" '
                    $0 == t { grab=1; next }
                    grab && /^----/ { exit }
                    grab && /^failures:/ { exit }
                    grab { print }
                ' "$out" | head -5 | tr '\n' ' ')"
                bad "$name" "${detail:-see cargo output above}"
                ;;
        esac
    done < "$out"
}

OUT="$TMP/cargo-test.out"
CARGO_TERM_COLOR=never "$CARGO_BIN" test --manifest-path "$ROOT/Cargo.toml" \
    -p panel -p loom --no-fail-fast > "$OUT" 2>&1
cat "$OUT"
_report_cargo_tests "$OUT"

tl_summary
