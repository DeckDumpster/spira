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

# No resolver/edition to maximise compatibility with the container's cargo version.
cat > "$WS/Cargo.toml" << 'EOF'
[workspace]
members = ["alpha-crate", "beta-crate"]
EOF

# Implicit binary named after the package (src/main.rs convention).
mkdir -p "$WS/alpha-crate/src"
printf '[package]\nname = "alpha-crate"\nversion = "0.1.0"\n' \
    > "$WS/alpha-crate/Cargo.toml"
printf 'fn main() {}\n' > "$WS/alpha-crate/src/main.rs"

mkdir -p "$WS/beta-crate/src"
printf '[package]\nname = "beta-crate"\nversion = "0.1.0"\n' \
    > "$WS/beta-crate/Cargo.toml"
printf 'fn main() {}\n' > "$WS/beta-crate/src/main.rs"

git -C "$WS" add .
git -C "$WS" commit -qm "fixture: initial workspace"

# Pre-populate target/release/ (build-tarball.sh does not run cargo).
# Binary names match package names (implicit convention).
mkdir -p "$WS/target/release"
printf '#!/usr/bin/env bash\necho alpha-crate\n' > "$WS/target/release/alpha-crate"
printf '#!/usr/bin/env bash\necho beta-crate\n'  > "$WS/target/release/beta-crate"
chmod +x "$WS/target/release/alpha-crate" "$WS/target/release/beta-crate"

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

if [ -x "${TREE1:-}/bin/alpha-crate" ]; then
    ok "bin/alpha-crate ships (auto-discovered)"
else
    bad "bin/alpha-crate ships (auto-discovered)" "not found; build: $build_out"
fi
if [ -x "${TREE1:-}/bin/beta-crate" ]; then
    ok "bin/beta-crate ships (auto-discovered)"
else
    bad "bin/beta-crate ships (auto-discovered)" "not found; build: $build_out"
fi

# ============================================================================
echo
echo "2. Adding a throwaway crate ships its binary without any flag change"
# ============================================================================
mkdir -p "$WS/throwaway/src"
printf '[package]\nname = "throwaway"\nversion = "0.1.0"\n' \
    > "$WS/throwaway/Cargo.toml"
printf 'fn main() {}\n' > "$WS/throwaway/src/main.rs"

printf '[workspace]\nmembers = ["alpha-crate", "beta-crate", "throwaway"]\n' \
    > "$WS/Cargo.toml"

git -C "$WS" add .
git -C "$WS" commit -qm "fixture: add throwaway crate"

printf '#!/usr/bin/env bash\necho throwaway\n' > "$WS/target/release/throwaway"
chmod +x "$WS/target/release/throwaway"

build2_out="$(run_build build --workspace "$WS" --output "$TMP/out2" 2>&1)"
build2_rc=$?
is "build after adding throwaway exits 0" "0" "$build2_rc"

tarball2="$(find "$TMP/out2" -name 'spira-*.tar.gz' | head -1)"
UNPACK2="$TMP/unpack2"; mkdir -p "$UNPACK2"
[ -f "${tarball2:-}" ] && tar -xzf "$tarball2" -C "$UNPACK2"
stem2="${tarball2:+$(basename "${tarball2%.tar.gz}")}"
TREE2="${stem2:+$UNPACK2/$stem2}"

if [ -x "${TREE2:-}/bin/throwaway" ]; then
    ok "bin/throwaway ships without any list edit (auto-discovery)"
else
    bad "bin/throwaway ships without any list edit (auto-discovery)" \
        "not found; build: $build2_out"
fi
if [ -x "${TREE2:-}/bin/alpha-crate" ] && [ -x "${TREE2:-}/bin/beta-crate" ]; then
    ok "existing binaries alpha-crate and beta-crate still ship"
else
    bad "existing binaries alpha-crate and beta-crate still ship" "one or both missing"
fi

# ============================================================================
echo
echo "3. MANIFEST contains sha256 entries per binary"
# ============================================================================
if [ -f "${TREE2:-}/MANIFEST" ]; then
    if grep -q "^bin/throwaway " "$TREE2/MANIFEST"; then
        ok "MANIFEST has sha256 entry for throwaway"
    else
        bad "MANIFEST has sha256 entry for throwaway" \
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
rm -f "$WS/target/release/throwaway"
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
