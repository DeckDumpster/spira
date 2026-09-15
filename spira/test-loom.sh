#!/usr/bin/env bash
#
# test-loom.sh — build the endpoint fixture and run the Rust integration tests.
#
#   ./test-loom.sh
#
# WHERE IT RUNS: the TIMED set, found by the `spira/test-*.sh` glob and run by `suites.sh`,
# because `gate-suites` does not name it. The endpoint tests hit a real database and take
# several seconds; a gated check that costs seconds on every branch is a gate that grows to
# minutes without an argument. These run on a schedule instead, and a red files a bead.
#
# WHAT IS EXERCISED. The Rust integration tests in `loom/tests/endpoint.rs` drive the
# server over a real socket against a throwaway database — the budget, the cache, the
# error path, and the static routes. This file does two things the Rust test file cannot:
# build the throwaway fixture (creating and seeding the database), and wire LOOM_TEST_BD to
# the `bd` resolved by the harness configuration rather than whatever `bd` happens to be on
# the cargo runner's PATH.
#
# WHY THE BINARY MUST EXIST TO RUN. The integration tests compile against the library, but
# they do not verify the binary itself; this suite skips rather than building, so a fresh
# clone with no binary fails the timed run and files a bead rather than silently being
# absent from the record.
#
# defect: sp-wok.3
# covers: loom/*
# timeout: 120
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
LOOM_ROOT="$HERE/../loom"

pass=0; fail=0
ok()  { printf '  ok    %s\n' "$1"; pass=$((pass+1)); }
bad() { printf '  FAIL  %s\n        %s\n' "$1" "${2:-}"; fail=$((fail+1)); }

# Resolve cargo BEFORE sourcing testdb.sh: conf.sh overwrites PATH with the harness's own
# tool directories, which do not include ~/.cargo/bin. Capture the absolute path now so the
# cargo test invocation below does not need PATH to contain it.
CARGO_BIN="$(command -v cargo 2>/dev/null || true)"
# The harness PATH often omits ~/.cargo/bin; try the standard install location before
# concluding cargo is absent from this machine entirely.
if [ -z "$CARGO_BIN" ] && [ -x "$HOME/.cargo/bin/cargo" ]; then
    CARGO_BIN="$HOME/.cargo/bin/cargo"
fi
if [ -z "$CARGO_BIN" ]; then
    echo "SKIP test-loom: cargo not found on PATH or at ~/.cargo/bin — the Rust tests cannot run" >&2
    echo "     install Rust: https://rustup.rs/" >&2
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

# shellcheck source=/dev/null
. "$HERE/testdb.sh"
if ! testdb_available; then
    echo "SKIP test-loom: no fixture database reachable — endpoint tests did not run" >&2
    exit 77
fi

TMP="$(mktemp -d)"; trap 'testdb_drop >/dev/null 2>&1; rm -rf "$TMP"' EXIT INT TERM

testdb_up loom >/dev/null 2>&1 || { bad "fixture database" "testdb_up failed — cannot seed"; printf '\n  %d ok, %d failed\n' "$pass" "$fail"; exit 1; }

# Seed four beads: two open, one in-progress, one closed.
# Edges: sp-aaa→sp-ccc (blocks), sp-bbb→sp-ccc (parent-child), sp-bbb→sp-zzz (dropped —
# sp-zzz is closed). The test asserts count=3, edges=2, dropped_edges=1 — three counts each
# with a non-zero expected value, so a failure is distinguishable from "nothing was there."
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

# Route through the SPIRA_BD seam: testdb_up sets SPIRA_BD to the embedded binary so bdq
# and cargo-test children use the right engine; the fallback bare "bd" is reachable via PATH
# when the shared fixture put the symlink there.  testdb_available already verified bd is
# callable, so no second check here.
BD_BIN="${SPIRA_BD:-bd}"
ok "fixture database ready at $SPIRA_DB"

export LOOM_TEST_DB="$SPIRA_DB"
export LOOM_TEST_BD="$BD_BIN"

if out="$("$CARGO_BIN" test --manifest-path "$LOOM_ROOT/Cargo.toml" 2>&1)"; then
    ok "endpoint tests passed"
    printf '%s\n' "$out" | grep -E '^\s*(test |ok |FAILED|running)' | head -20 || true
else
    bad "endpoint tests" "$(printf '%s\n' "$out" | tail -30)"
fi

printf '\n  %d ok, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
