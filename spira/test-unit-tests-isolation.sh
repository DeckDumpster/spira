#!/usr/bin/env bash
#
# test-unit-tests-isolation.sh — unit-tests.sh never reports a verdict built from another tree.
#
# Cargo decides freshness by mtime, so on a target shared between trees at one checkout path, a
# tree whose files predate the last build of a different tree reuses that tree's artifacts.
# Two trees that differ in one test name, and in what their library returns, run in sequence at
# one path: on one shared target (a testenv that names no volume, cases 1-5), and each on its
# own (UNIT_TARGET_VOLUME, case 6). Each verdict must list only its own tree's test,
# built against its own library; on its own target a tree tested again rebuilds nothing.
#
# tier: T2
# covers: spira/unit-tests.sh
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
    cp "$HERE/unit-tests.sh" "$WS/spira/unit-tests.sh"
    printf '[workspace]\nmembers = ["lc", "app"]\nresolver = "2"\n' > "$WS/Cargo.toml"
    printf '[package]\nname = "lc"\nversion = "0.1.0"\nedition = "2021"\n' > "$WS/lc/Cargo.toml"
    printf '[package]\nname = "app"\nversion = "0.1.0"\nedition = "2021"\n\n[dependencies]\nlc = { path = "../lc" }\n' > "$WS/app/Cargo.toml"
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
mkdir -p "$WS/sealed" && printf 'x\n' > "$WS/sealed/f" && chmod 000 "$WS/sealed/f"
if [ -r "$WS/sealed/f" ]; then
    bad "an unreadable file is needed to prove the fallback" "running as uid $(id -u), which reads every file"
fi
out="$(run_unit)"; rc=$?
chmod 600 "$WS/sealed/f"
wantrc "tree A passes" 0 "$rc"
want "after recompiling the workspace" "Compiling app" "$out"
chmod -R u+rwx "$WS" 2>/dev/null

# run_own <volume>: testenv's unit tier since sp-fnb2wa — each tree on its own target volume,
# named to the script, which then has nothing to clean.
run_own() {
    (cd "$WS" && env CARGO_TARGET_DIR="$TMP/$1" UNIT_TARGET_VOLUME="$1" bash spira/unit-tests.sh 2>&1)
}

echo "6. trees A, B, A, each on its own target: the second A is warm and judged on A:"
put_tree a alpha_only
out="$(run_own vol-a)"; rc=$?
wantrc "tree A passes" 0 "$rc"
want "and lists A's test" "test tests::alpha_only ... ok" "$out"
put_tree b beta_only
backdate
out="$(run_own vol-b)"; rc=$?
wantrc "tree B passes against its own library" 0 "$rc"
want "and lists B's test" "test tests::beta_only ... ok" "$out"
nowant "and not A's" "alpha_only" "$out"
put_tree a alpha_only
backdate
out="$(run_own vol-a)"; rc=$?
wantrc "tree A passes again" 0 "$rc"
want "and lists A's test" "test tests::alpha_only ... ok" "$out"
nowant "and not B's" "beta_only" "$out"
nowant "without recompiling the workspace" "Compiling app" "$out"
[ ! -e "$TMP/vol-a/.unit-tests-built-tree" ] && ok "and without hashing the tree for a clean" || bad "and without hashing the tree for a clean" "$TMP/vol-a/.unit-tests-built-tree exists"

tl_summary
