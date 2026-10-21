#!/usr/bin/env bash
#
# test-tarball-bins.sh — every binary named in a non-optional unit's ExecStart
# is shipped in the tarball; build-tarball.sh refuses to build with no binary source.
#
# The structural claim — that non-optional units name at least one release binary —
# and its own positive control now live in test-timer-templates.sh's T0 unit-lint, which
# runs without paying for a build. What is left here needs the built tarball itself:
#
# CASES
#   1. POSITIVE CONTROL: build with neither --bin-dir nor --workspace exits non-zero.
#   2. Build from a --bin-dir: tarball contains bin/spira-supervise and its siblings.
#   3. Each @SPIRA_PROD_ROOT@/bin/<name> in a non-optional unit's ExecStart is a binary
#      present in the built tarball.
#   4. release.yml names a 'Build supervise' step.
#
# tier: T1
# covers: spira/build-tarball.sh .github/workflows/release.yml systemd/*.service install/src/manifest.rs UC-instance-lifecycle-03
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
REPO_ROOT="$(cd "$HERE/.." && pwd -P)"

echo "test-tarball-bins.sh"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM

export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t
export GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

# ---------------------------------------------------------------------------
# A unit names the release binary it runs as @SPIRA_PROD_ROOT@/bin/<name> (sp-gypjk): the
# name in the path IS the bin/ entry, so there is no token table to keep in step.
# ---------------------------------------------------------------------------
# OPTIONAL units, derived from units-install itself (sp-31dm0: systemd/units.sh is
# retired; not a hand-copied list — the old list silently fell out of sync, missing
# spira-landing-pass.service/.timer). `--list-optional` is the UNION of every template any
# combination of this box's conditional inputs can ever push into OPTIONAL, regardless of
# which this box happens to evaluate true, matching test-timer-templates.sh's
# UNITS/_ENABLE_TMPL parse.
OPTIONAL_BLOCK="$(units-install --list-optional)"
is_optional() { case "$OPTIONAL_BLOCK" in *"$1"*) return 0 ;; *) return 1 ;; esac; }

# ---------------------------------------------------------------------------
# Scan: collect the bin/<name> each non-optional unit's ExecStart runs. The positive
# control for this scan (at least one token found) lives in test-timer-templates.sh, which
# runs the same scan without needing a build.
declare -A FOUND_TOKENS=()
while IFS= read -r svc; do
    fname="$(basename "$svc")"
    is_optional "$fname" && continue
    while IFS= read -r line; do
        [[ "$line" =~ ^ExecStart ]] || continue
        for tok in $(printf '%s' "$line" | grep -oE '@SPIRA_PROD_ROOT@/bin/[a-z0-9-]+' | sed 's#.*/bin/##'); do
            FOUND_TOKENS[$tok]=1
        done
    done < "$svc"
done < <(find "$REPO_ROOT/systemd" -name '*.service' 2>/dev/null)

# ---------------------------------------------------------------------------
# GIT FIXTURE
# ---------------------------------------------------------------------------
ORIGIN="$TMP/origin.git"
REPO="$TMP/repo"
git init -q --bare -b main "$ORIGIN"
timeout 5 git clone -q "$ORIGIN" "$REPO" 2>/dev/null
git -C "$REPO" config user.email t@t
git -C "$REPO" config user.name t
mkdir -p "$REPO/spira"
printf '#!/usr/bin/env bash\necho install\n' > "$REPO/install.sh"
printf 'key=val\n' > "$REPO/spira/conf.sh"
chmod +x "$REPO/install.sh"
git -C "$REPO" add .
git -C "$REPO" commit -qm "fixture: initial"
timeout 5 git -C "$REPO" push -q origin main 2>/dev/null

# ---------------------------------------------------------------------------
# BINARY STUBS
# ---------------------------------------------------------------------------
mkdir -p "$TMP/bins"
for _b in loom panel broker spira-supervise landing-pass reconciler-flow; do
    printf '#!/usr/bin/env bash\necho %s\n' "$_b" > "$TMP/bins/$_b"
    chmod +x "$TMP/bins/$_b"
done
unset _b

run_build() {
    env -i \
        PATH="$PATH" \
        HOME="$TMP/home" \
        GIT_CONFIG_GLOBAL=/dev/null \
        GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t \
        GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t \
        bash "$HERE/build-tarball.sh" "$@" 2>&1
    return $?
}
mkdir -p "$TMP/home" "$TMP/out-full"

# ============================================================================
echo
echo "1. POSITIVE CONTROL — build with no binary source exits non-zero"
# ============================================================================
no_src_rc=0
run_build build \
    --output "$TMP/out-nosrc" \
    HEAD "$REPO" >/dev/null 2>&1 || no_src_rc=$?
if [ "$no_src_rc" -ne 0 ]; then
    ok "build with neither --bin-dir nor --workspace exits non-zero (exit $no_src_rc)"
else
    bad "build with neither --bin-dir nor --workspace exits non-zero" \
        "exited 0 — a tarball with no binaries was built"
fi

# ============================================================================
echo
echo "2. Build from --bin-dir — bin/spira-supervise, bin/landing-pass and bin/reconciler-flow are present"
# ============================================================================
build_out="$(run_build build \
    --output "$TMP/out-full" \
    --bin-dir "$TMP/bins" \
    HEAD "$REPO" 2>&1)"
build_rc=$?
is "build from --bin-dir exits 0" "0" "$build_rc"

tarball="$(find "$TMP/out-full" -name 'spira-*.tar.gz' | head -1)"
UNPACK="$TMP/unpack"
mkdir -p "$UNPACK"
if [ -n "${tarball:-}" ] && [ -f "$tarball" ]; then
    tar -xzf "$tarball" -C "$UNPACK"
fi
stem="${tarball:+$(basename "${tarball%.tar.gz}")}"
TREE="${stem:+$UNPACK/$stem}"

if [ -x "${TREE:-}/bin/spira-supervise" ]; then
    ok "bin/spira-supervise is present and executable"
else
    bad "bin/spira-supervise is present and executable" \
        "not found or not executable (build output: $build_out)"
fi
if [ -x "${TREE:-}/bin/landing-pass" ]; then
    ok "bin/landing-pass is present and executable"
else
    bad "bin/landing-pass is present and executable" \
        "not found or not executable (build output: $build_out)"
fi
if [ -x "${TREE:-}/bin/reconciler-flow" ]; then
    ok "bin/reconciler-flow is present and executable"
else
    bad "bin/reconciler-flow is present and executable" \
        "not found or not executable (build output: $build_out)"
fi

# ============================================================================
echo
echo "3. Each bin/<name> a non-optional ExecStart runs is in the tarball"
# ============================================================================
# This case builds its own tarball from a --bin-dir holding one stub per ExecStart token,
# and checks every token against THAT tree.
mkdir -p "$TMP/bindir" "$TMP/out-bindir"
for _b in "${!FOUND_TOKENS[@]}"; do
    printf '#!/usr/bin/env bash\necho %s\n' "$_b" > "$TMP/bindir/$_b"
    chmod +x "$TMP/bindir/$_b"
done
unset _b
bindir_out="$(run_build build --output "$TMP/out-bindir" --bin-dir "$TMP/bindir" HEAD "$REPO" 2>&1)"
bindir_rc=$?
is "build with --bin-dir exits 0" "0" "$bindir_rc"
[ "$bindir_rc" = 0 ] || printf '# %s\n' "$bindir_out"
tarball3="$(find "$TMP/out-bindir" -name 'spira-*.tar.gz' | head -1)"
UNPACK3="$TMP/unpack-bindir"
mkdir -p "$UNPACK3"
[ -n "${tarball3:-}" ] && [ -f "$tarball3" ] && tar -xzf "$tarball3" -C "$UNPACK3"
stem3="${tarball3:+$(basename "${tarball3%.tar.gz}")}"
TREE3="${stem3:+$UNPACK3/$stem3}"
for bin_name in "${!FOUND_TOKENS[@]}"; do
    if [ -x "${TREE3:-}/bin/$bin_name" ]; then
        ok "bin/$bin_name is present in the tarball"
    else
        bad "bin/$bin_name is present in the tarball" \
            "not found in tarball — add it to build-tarball.sh and release.yml"
    fi
done

# ============================================================================
echo
echo "4. release.yml builds with 'make build' (no per-crate build steps)"
# ============================================================================
RELEASE_YML="$REPO_ROOT/.github/workflows/release.yml"
if [ -f "$RELEASE_YML" ] && grep -q "make build" "$RELEASE_YML"; then
    ok "release.yml uses 'make build'"
else
    bad "release.yml uses 'make build'" \
        "not found — per-crate build steps miss newly added crates"
fi

tl_summary
