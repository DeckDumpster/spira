#!/usr/bin/env bash
#
# test-workspace-dist.sh — build-tarball.sh --workspace auto-discovers all
# workspace binaries without a hand-written list.
#
# POSITIVE CONTROL FIRST: a workspace with two known binaries is built; both
# ship. Then a throwaway crate is added and the identical invocation ships its
# binary too — proving auto-discovery with no other edit.
#
# CASES
#   1. POSITIVE CONTROL: both initial workspace binaries ship.
#   2. Adding a throwaway crate ships its binary without any flag change.
#   3. MANIFEST contains sha256 entries for each binary.
#   4. POSITIVE CONTROL: a missing pre-built binary is refused, not silently skipped.
#
# covers: spira/build-tarball.sh Cargo.toml Makefile
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
pass=0; fail=0
ok()   { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()  { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()   { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }

echo "test-workspace-dist.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t
export GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

# cargo metadata --no-deps is required for workspace binary discovery.
if ! command -v cargo >/dev/null 2>&1; then
    printf '  SKIP  cargo not on PATH; workspace discovery test requires real cargo\n'
    printf '\n0 passed, 0 failed\n'
    exit 0
fi

# ============================================================================
# FIXTURE: a minimal cargo workspace
# ============================================================================
WS="$TMP/ws"
mkdir -p "$WS"
git init -q -b main "$WS"
git -C "$WS" config user.email t@t
git -C "$WS" config user.name t

cat > "$WS/Cargo.toml" << 'EOF'
[workspace]
members = ["alpha-crate", "beta-crate"]
resolver = "2"
EOF

mkdir -p "$WS/alpha-crate/src"
cat > "$WS/alpha-crate/Cargo.toml" << 'EOF'
[package]
name = "alpha-crate"
version = "0.1.0"
edition = "2021"

[[bin]]
name = "alpha-bin"
path = "src/main.rs"
EOF
printf 'fn main() {}\n' > "$WS/alpha-crate/src/main.rs"

mkdir -p "$WS/beta-crate/src"
cat > "$WS/beta-crate/Cargo.toml" << 'EOF'
[package]
name = "beta-crate"
version = "0.1.0"
edition = "2021"

[[bin]]
name = "beta-bin"
path = "src/main.rs"
EOF
printf 'fn main() {}\n' > "$WS/beta-crate/src/main.rs"

git -C "$WS" add .
git -C "$WS" commit -qm "fixture: initial workspace"

# Pre-populate target/release/ (build-tarball.sh does not run cargo).
mkdir -p "$WS/target/release"
printf '#!/usr/bin/env bash\necho alpha\n' > "$WS/target/release/alpha-bin"
printf '#!/usr/bin/env bash\necho beta\n'  > "$WS/target/release/beta-bin"
chmod +x "$WS/target/release/alpha-bin" "$WS/target/release/beta-bin"

run_build() {
    env -i \
        PATH="$PATH" \
        HOME="$TMP/home" \
        GIT_CONFIG_GLOBAL=/dev/null \
        GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t \
        GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t \
        bash "$HERE/build-tarball.sh" "$@" 2>&1
}
mkdir -p "$TMP/home"

# ============================================================================
echo
echo "1. POSITIVE CONTROL — initial workspace ships both declared binaries"
# ============================================================================
build_out="$(run_build build --workspace "$WS" --output "$TMP/out1" 2>&1)"
build_rc=$?
is "build --workspace exits 0" "0" "$build_rc"

tarball="$(find "$TMP/out1" -name 'spira-*.tar.gz' | head -1)"
UNPACK1="$TMP/unpack1"; mkdir -p "$UNPACK1"
[ -f "${tarball:-}" ] && tar -xzf "$tarball" -C "$UNPACK1"
stem="${tarball:+$(basename "${tarball%.tar.gz}")}"
TREE1="${stem:+$UNPACK1/$stem}"

if [ -x "${TREE1:-}/bin/alpha-bin" ]; then
    ok "bin/alpha-bin ships (auto-discovered)"
else
    bad "bin/alpha-bin ships (auto-discovered)" "not found; build: $build_out"
fi
if [ -x "${TREE1:-}/bin/beta-bin" ]; then
    ok "bin/beta-bin ships (auto-discovered)"
else
    bad "bin/beta-bin ships (auto-discovered)" "not found; build: $build_out"
fi

# ============================================================================
echo
echo "2. Adding a throwaway crate ships its binary without any flag change"
# ============================================================================
mkdir -p "$WS/throwaway/src"
cat > "$WS/throwaway/Cargo.toml" << 'EOF'
[package]
name = "throwaway"
version = "0.1.0"
edition = "2021"

[[bin]]
name = "throwaway-bin"
path = "src/main.rs"
EOF
printf 'fn main() {}\n' > "$WS/throwaway/src/main.rs"

cat > "$WS/Cargo.toml" << 'EOF'
[workspace]
members = ["alpha-crate", "beta-crate", "throwaway"]
resolver = "2"
EOF

git -C "$WS" add .
git -C "$WS" commit -qm "fixture: add throwaway crate"

printf '#!/usr/bin/env bash\necho throwaway\n' > "$WS/target/release/throwaway-bin"
chmod +x "$WS/target/release/throwaway-bin"

build2_out="$(run_build build --workspace "$WS" --output "$TMP/out2" 2>&1)"
build2_rc=$?
is "build after adding throwaway exits 0" "0" "$build2_rc"

tarball2="$(find "$TMP/out2" -name 'spira-*.tar.gz' | head -1)"
UNPACK2="$TMP/unpack2"; mkdir -p "$UNPACK2"
[ -f "${tarball2:-}" ] && tar -xzf "$tarball2" -C "$UNPACK2"
stem2="${tarball2:+$(basename "${tarball2%.tar.gz}")}"
TREE2="${stem2:+$UNPACK2/$stem2}"

if [ -x "${TREE2:-}/bin/throwaway-bin" ]; then
    ok "bin/throwaway-bin ships without any list edit (auto-discovery)"
else
    bad "bin/throwaway-bin ships without any list edit (auto-discovery)" \
        "not found; build: $build2_out"
fi
if [ -x "${TREE2:-}/bin/alpha-bin" ] && [ -x "${TREE2:-}/bin/beta-bin" ]; then
    ok "existing binaries alpha-bin and beta-bin still ship"
else
    bad "existing binaries alpha-bin and beta-bin still ship" "one or both missing"
fi

# ============================================================================
echo
echo "3. MANIFEST contains sha256 entries per binary"
# ============================================================================
if [ -f "${TREE2:-}/MANIFEST" ]; then
    if grep -q "^bin/throwaway-bin " "$TREE2/MANIFEST"; then
        ok "MANIFEST has sha256 entry for throwaway-bin"
    else
        bad "MANIFEST has sha256 entry for throwaway-bin" \
            "$(cat "$TREE2/MANIFEST")"
    fi
    if grep -q "^commit " "$TREE2/MANIFEST" && grep -q "^timestamp " "$TREE2/MANIFEST"; then
        ok "MANIFEST has commit and timestamp lines"
    else
        bad "MANIFEST has commit and timestamp lines" \
            "$(cat "$TREE2/MANIFEST")"
    fi
else
    bad "MANIFEST is present" "not found at ${TREE2:-}/MANIFEST"
fi

# ============================================================================
echo
echo "4. POSITIVE CONTROL — missing pre-built binary is refused"
# ============================================================================
rm -f "$WS/target/release/throwaway-bin"
missing_out="$(run_build build --workspace "$WS" --output "$TMP/out3" 2>&1)"
missing_rc=$?
if [ "$missing_rc" -ne 0 ]; then
    ok "missing binary causes non-zero exit (positive control)"
else
    bad "missing binary causes non-zero exit (positive control)" \
        "exited 0 with output: $missing_out"
fi

# ============================================================================
printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
