#!/usr/bin/env bash
#
# test-unit-tests-isolation.sh — unit-tests.sh never reports a verdict built from another tree.
#
# Every unit-tests batch mounts one cargo target volume at the same checkout path, and cargo
# decides freshness by mtime, so a tree whose files predate the last build of a different tree
# reuses that tree's artifacts. Two trees that differ in one test name, and in what their
# library returns, run in sequence on one target dir, at one path; each verdict must list only
# its own tree's test, built against its own library.
#
# tier: T2
# covers: spira/unit-tests.sh spira/target-clean.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

echo "test-unit-tests-isolation.sh"

TMP="$(mktemp -d)"; trap 'chmod -R u+rwx "$TMP" 2>/dev/null; rm -rf "$TMP"' EXIT INT TERM
WS="$TMP/workspace"
VOL="$TMP/volume"

# put_tree <lib-tag> <test-name>: the checkout at $WS becomes that tree, as the host leaves it
# for the container — crate list and test-bins list under target/.
put_tree() {
    rm -rf "$WS"
    mkdir -p "$WS/spira" "$WS/lc/src" "$WS/app/src" "$WS/target"
    cp "$HERE/unit-tests.sh" "$HERE/target-clean.sh" "$WS/spira/"
    printf '[workspace]\nmembers = ["lc", "app"]\nresolver = "2"\n' > "$WS/Cargo.toml"
    printf '[package]\nname = "lc"\nversion = "0.1.0"\nedition = "2021"\n\n[features]\nintegration = []\n' > "$WS/lc/Cargo.toml"
    printf '[package]\nname = "app"\nversion = "0.1.0"\nedition = "2021"\n\n[features]\nintegration = []\n\n[dependencies]\nlc = { path = "../lc" }\n' > "$WS/app/Cargo.toml"
    printf 'pub fn tag() -> &'"'"'static str { "%s" }\n' "$1" > "$WS/lc/src/lib.rs"
    printf '#[cfg(test)]\nmod tests {\n    #[test]\n    fn %s() {\n        assert_eq!(lc::tag(), "%s");\n    }\n}\n' "$2" "$1" > "$WS/app/src/lib.rs"
    (cd "$WS" && cargo generate-lockfile --offline >/dev/null 2>&1)
    printf 'app\n' > "$WS/target/unit-tests.crates"
    : > "$WS/target/unit-tests.bins"
}

# A tree whose files are older than the last build of another tree: a worktree edited before
# that build ran, or a warm slot checked out without touching unchanged files.
backdate() {
    find "$WS" -path "$WS/target" -prune -o -type f -exec touch -d '2001-01-01' {} +
}

run_unit() {
    (cd "$WS" && env CARGO_TARGET_DIR="$VOL" bash spira/unit-tests.sh 2>&1)
}

echo "1. tree A builds and lists its own test:"
put_tree a alpha_only
out="$(run_unit)"; rc=$?
wantrc "tree A passes" 0 "$rc"
want "and lists A's test" "test tests::alpha_only ... ok" "$out"

echo "2. tree B, older on disk, at the same path on the same target, is judged on B:"
put_tree b beta_only
backdate
out="$(run_unit)"; rc=$?
wantrc "tree B passes against its own library" 0 "$rc"
want "and lists B's test" "test tests::beta_only ... ok" "$out"
nowant "and not A's, from A's test binary" "alpha_only" "$out"

echo "3. tree B again keeps the warm target:"
out="$(run_unit)"; rc=$?
wantrc "tree B passes again" 0 "$rc"
nowant "without recompiling the workspace" "Compiling app" "$out"

echo "4. tree A, older on disk than B's build, is judged on A:"
put_tree a alpha_only
backdate
out="$(run_unit)"; rc=$?
wantrc "tree A passes against its own library" 0 "$rc"
want "and lists A's test" "test tests::alpha_only ... ok" "$out"
nowant "and not B's" "beta_only" "$out"

echo "5. a tree that cannot be hashed is cleaned, never trusted:"
# Inside a workspace crate: the record hashes the crates' own files, the build inputs and what
# their dep-info names (sp-ipwlic: a file outside every crate is no build input, and is not read).
mkdir -p "$WS/app/sealed" && printf 'x\n' > "$WS/app/sealed/f" && chmod 000 "$WS/app/sealed/f"
if [ -r "$WS/app/sealed/f" ]; then
    bad "an unreadable file is needed to prove the fallback" "running as uid $(id -u), which reads every file"
fi
out="$(run_unit)"; rc=$?
chmod 600 "$WS/app/sealed/f"
wantrc "tree A passes" 0 "$rc"
want "after recompiling the workspace" "Compiling app" "$out"

echo "6. the tier shares the host: 6 jobs and 4 test threads by default, the env overrides them, temp on /tmp:"
put_tree a alpha_only
FAKE="$TMP/fakebin"; mkdir -p "$FAKE"
printf '#!/bin/sh\n[ "$1" = test ] && echo "fake-cargo $* TMPDIR=$TMPDIR"\nexit 0\n' > "$FAKE/cargo"; chmod +x "$FAKE/cargo"
fake_run() { (cd "$WS" && env PATH="$FAKE:$PATH" CARGO_TARGET_DIR="$VOL" "$@" bash spira/unit-tests.sh 2>&1); }
out="$(fake_run env -u CARGO_BUILD_JOBS -u RUST_TEST_THREADS)"
want "6 cargo jobs by default" "-j 6 --" "$out"
want "4 test threads by default" "--test-threads=4" "$out"
want "integration targets are opted in" "--features integration" "$out"
want "test temp files on the tmpfs" "TMPDIR=/tmp" "$out"
out="$(fake_run env CARGO_BUILD_JOBS=2 RUST_TEST_THREADS=1)"
want "CARGO_BUILD_JOBS overrides the jobs" "-j 2 --" "$out"
want "RUST_TEST_THREADS overrides the threads" "--test-threads=1" "$out"

tl_summary
