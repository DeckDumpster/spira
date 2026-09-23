#!/usr/bin/env bash
#
# test-tarball-bins.sh — every binary named in a non-optional unit's ExecStart
# is shipped in the tarball; build-tarball.sh enforces all four required bins.
#
# POSITIVE CONTROL FIRST (law-absence-needs-a-positive-control):
# We first confirm at least one @*_BIN@ token appears in a non-optional unit's
# ExecStart (so silence is not vacuous success), and that building without
# --supervise-bin is refused (so the requirement is actually enforced).
#
# CASES
#   1. POSITIVE CONTROL: at least one non-optional unit references @*_BIN@ in ExecStart.
#   2. POSITIVE CONTROL: build without --supervise-bin exits non-zero.
#   3. Build with all four bins: tarball contains bin/spira-supervise.
#   4. Each @*_BIN@ token found in non-optional unit ExecStart lines corresponds to
#      a binary present in the built tarball.
#   5. release.yml names a 'Build supervise' step.
#
# WHAT WOULD HAVE CAUGHT sp-mplcb:
#   sp-mplcb added @SPIRA_SUPERVISE_BIN@ to spira-cockpit.service ExecStart without
#   adding --supervise-bin to build-tarball.sh. Case 2 (build refused without
#   --supervise-bin) or case 3 (bin/spira-supervise absent from tarball) would have
#   been RED. Case 4 provides the structural claim for any future addition.
#
# covers: spira/build-tarball.sh .github/workflows/release.yml systemd/*.service
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
REPO_ROOT="$(cd "$HERE/.." && pwd -P)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }

echo "test-tarball-bins.sh"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM

export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t
export GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

# ---------------------------------------------------------------------------
# Mapping table: @TOKEN@ → bin/<name>.
# Add an entry here whenever a new @*_BIN@ token appears in a unit ExecStart.
# If this table is missing a token found in systemd/*.service, case 4 reports
# it as unknown and the test fails — forcing the developer to update both this
# table and build-tarball.sh together.
declare -A TOKEN_TO_BIN=(
    [SPIRA_SUPERVISE_BIN]=spira-supervise
)

# ---------------------------------------------------------------------------
# OPTIONAL units (keep in sync with systemd/units.sh OPTIONAL logic).
# A unit in this list is excluded from the ExecStart scan.
OPTIONAL_UNITS=(
    spira-promote.service spira-promote.timer
    spira-suites.service spira-suites.timer
    spira-mail-deliver.service
    dolt-beads.service
    dolt-beads-test.service
    spira-loom.service
    spira-broker.service spira-broker.timer
)

is_optional() {
    local u="$1"
    for o in "${OPTIONAL_UNITS[@]}"; do [ "$u" = "$o" ] && return 0; done
    return 1
}

# ---------------------------------------------------------------------------
# Scan: collect @*_BIN@ tokens from ExecStart lines of non-optional units.
declare -A FOUND_TOKENS=()
while IFS= read -r svc; do
    fname="$(basename "$svc")"
    is_optional "$fname" && continue
    while IFS= read -r line; do
        [[ "$line" =~ ^ExecStart ]] || continue
        for tok in $(printf '%s' "$line" | grep -oE '@[A-Z_]+_BIN@' | tr -d '@'); do
            FOUND_TOKENS[$tok]=1
        done
    done < "$svc"
done < <(find "$REPO_ROOT/systemd" -name '*.service' 2>/dev/null)

# ============================================================================
echo
echo "1. POSITIVE CONTROL — at least one non-optional unit references @*_BIN@ in ExecStart"
# ============================================================================
if [ "${#FOUND_TOKENS[@]}" -gt 0 ]; then
    ok "found ${#FOUND_TOKENS[@]} @*_BIN@ token(s): ${!FOUND_TOKENS[*]}"
else
    bad "at least one @*_BIN@ token in non-optional ExecStart" \
        "none found — either all units are optional or ExecStart references were removed; positive control failed"
fi

# ---------------------------------------------------------------------------
# GIT FIXTURE
# ---------------------------------------------------------------------------
ORIGIN="$TMP/origin.git"
REPO="$TMP/repo"
git init -q --bare -b main "$ORIGIN"
git clone -q "$ORIGIN" "$REPO" 2>/dev/null
git -C "$REPO" config user.email t@t
git -C "$REPO" config user.name t
mkdir -p "$REPO/spira"
printf '#!/usr/bin/env bash\necho install\n' > "$REPO/install.sh"
printf 'key=val\n' > "$REPO/spira/conf.sh"
chmod +x "$REPO/install.sh"
git -C "$REPO" add .
git -C "$REPO" commit -qm "fixture: initial"
git -C "$REPO" push -q origin main 2>/dev/null

# ---------------------------------------------------------------------------
# BINARY STUBS
# ---------------------------------------------------------------------------
mkdir -p "$TMP/bins"
LOOM_BIN="$TMP/bins/loom"
PANEL_BIN="$TMP/bins/panel"
BROKER_BIN="$TMP/bins/broker"
SUPERVISE_BIN="$TMP/bins/spira-supervise"
printf '#!/usr/bin/env bash\necho loom\n'            > "$LOOM_BIN"
printf '#!/usr/bin/env bash\necho panel\n'           > "$PANEL_BIN"
printf '#!/usr/bin/env bash\necho broker\n'          > "$BROKER_BIN"
printf '#!/usr/bin/env bash\necho spira-supervise\n' > "$SUPERVISE_BIN"
chmod +x "$LOOM_BIN" "$PANEL_BIN" "$BROKER_BIN" "$SUPERVISE_BIN"

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
echo "2. POSITIVE CONTROL — build without --supervise-bin exits non-zero"
# ============================================================================
# Before the fix, build-tarball.sh did not require --supervise-bin and would
# exit 0 here, causing this positive control to fail (test RED before fix).
no_sup_rc=0
run_build build \
    --output "$TMP/out-nosup" \
    --loom-bin "$LOOM_BIN" \
    --panel-bin "$PANEL_BIN" \
    --broker-bin "$BROKER_BIN" \
    HEAD "$REPO" >/dev/null 2>&1 || no_sup_rc=$?
if [ "$no_sup_rc" -ne 0 ]; then
    ok "build without --supervise-bin exits non-zero (exit $no_sup_rc)"
else
    bad "build without --supervise-bin exits non-zero" \
        "exited 0 — --supervise-bin is not yet enforced (test RED before fix; should be RED against today's code)"
fi

# ============================================================================
echo
echo "3. Build with all four bins — bin/spira-supervise is present"
# ============================================================================
build_out="$(run_build build \
    --output "$TMP/out-full" \
    --loom-bin "$LOOM_BIN" \
    --panel-bin "$PANEL_BIN" \
    --broker-bin "$BROKER_BIN" \
    --supervise-bin "$SUPERVISE_BIN" \
    HEAD "$REPO" 2>&1)"
build_rc=$?
is "build with all bins exits 0" "0" "$build_rc"

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

# ============================================================================
echo
echo "4. Each @*_BIN@ token in non-optional ExecStart maps to a bin/ in the tarball"
# ============================================================================
for tok in "${!FOUND_TOKENS[@]}"; do
    bin_name="${TOKEN_TO_BIN[$tok]:-}"
    if [ -z "$bin_name" ]; then
        bad "token @${tok}@ has a known bin/ mapping" \
            "not in TOKEN_TO_BIN table — update test-tarball-bins.sh and build-tarball.sh together"
        continue
    fi
    if [ -x "${TREE:-}/bin/$bin_name" ]; then
        ok "bin/$bin_name is present for @${tok}@"
    else
        bad "bin/$bin_name is present for @${tok}@" \
            "not found in tarball — add it to build-tarball.sh and release.yml"
    fi
done

# ============================================================================
echo
echo "5. release.yml contains a 'Build supervise' step"
# ============================================================================
RELEASE_YML="$REPO_ROOT/.github/workflows/release.yml"
if [ -f "$RELEASE_YML" ] && grep -q "Build supervise" "$RELEASE_YML"; then
    ok "release.yml has 'Build supervise' step"
else
    bad "release.yml has 'Build supervise' step" \
        "step absent — supervise binary will not be built in CI"
fi

# ============================================================================
printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
