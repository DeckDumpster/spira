#!/usr/bin/env bash
#
# test-build-bd-release.sh — build-bd.sh --from-release: naming convention and checksum guard
#
# CASES
#   1. POSITIVE CONTROL: checksum mismatch causes hard rejection before unpacking.
#   2. Correct beads_${TAG#v}_${os}_${arch}.tar.gz naming with matching checksums.txt succeeds.
#
# BD_RELEASE_BASE_URL exercises the naming derivation: the code constructs the asset name;
# a regression back to bd_${os}_${arch}.tar.gz breaks this suite because the stub only
# serves the beads_... file.
#
# tier: T1
# covers: spira/build-bd.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"

echo "test-build-bd-release.sh"

command -v curl      >/dev/null 2>&1 || { printf 'SKIP test-build-bd-release.sh: curl not found\n' >&2; exit 77; }
command -v sha256sum >/dev/null 2>&1 || { printf 'SKIP test-build-bd-release.sh: sha256sum not found\n' >&2; exit 77; }

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM

TAG="$(grep '^BD_TAG_PIN=' "$HERE/build-bd.sh" | head -1 | cut -d'"' -f2)"
[ -n "$TAG" ] || { printf 'SKIP test-build-bd-release.sh: could not read BD_TAG_PIN\n' >&2; exit 77; }

_os="$(uname -s | tr '[:upper:]' '[:lower:]')"
_arch="$(uname -m)"
case "$_arch" in
    x86_64)  _arch=amd64 ;;
    aarch64) _arch=arm64 ;;
    *)       printf 'SKIP test-build-bd-release.sh: unsupported arch %s\n' "$_arch" >&2; exit 77 ;;
esac
ASSET="beads_${TAG#v}_${_os}_${_arch}.tar.gz"

# Stub bd binary: exits 0 for every subcommand (unclaim/reclaim/heartbeat/sync/schema/conflicts
# --help and init --non-interactive all exit 0, satisfying build-bd.sh's verification block).
mkdir -p "$TMP/bd-src"
printf '#!/bin/sh\nexit 0\n' > "$TMP/bd-src/bd"
chmod +x "$TMP/bd-src/bd"

# Stub directory: the naming-derivation test only serves the beads_... file.
STUB_DIR="$TMP/serve"
mkdir -p "$STUB_DIR"
tar -czf "$STUB_DIR/$ASSET" -C "$TMP/bd-src" bd
(cd "$STUB_DIR" && sha256sum "$ASSET" > checksums.txt)

FAKE_HOME="$TMP/home"
mkdir -p "$FAKE_HOME/.local/bin"

# ============================================================================
echo ""
echo "1. POSITIVE CONTROL — checksum mismatch is rejected"
# ============================================================================
TAMPER_DIR="$TMP/tamper"
mkdir -p "$TAMPER_DIR"
cp "$STUB_DIR/$ASSET" "$TAMPER_DIR/$ASSET"
printf '0000000000000000000000000000000000000000000000000000000000000000  %s\n' \
    "$ASSET" > "$TAMPER_DIR/checksums.txt"

tamper_out="$(env -i PATH="$PATH" HOME="$FAKE_HOME" \
    BD_RELEASE_BASE_URL="file://$TAMPER_DIR" \
    bash "$HERE/build-bd.sh" --from-release 2>&1)"
tamper_rc=$?

if [ "$tamper_rc" -ne 0 ]; then
    ok "checksum mismatch causes non-zero exit"
else
    bad "checksum mismatch causes non-zero exit" "exited 0 — positive control failed"
fi
want "checksum mismatch reported in output" "checksum mismatch" "$tamper_out"

# ============================================================================
echo ""
echo "2. Correct naming and checksums — download succeeds"
# ============================================================================
# Assert the stub has the correctly-named file; without this the success in the
# run below could be vacuous (law-absence-needs-a-positive-control).
if [ -f "$STUB_DIR/$ASSET" ]; then
    ok "stub serves $ASSET (naming positive control)"
else
    bad "stub serves $ASSET" "file not found — test cannot proceed"
    printf '%s passed, %s failed\n' "$_TL_PASS" "$_TL_FAIL"; [ "$_TL_FAIL" = 0 ]; exit
fi

rel_out="$(env -i PATH="$PATH" HOME="$FAKE_HOME" \
    BD_RELEASE_BASE_URL="file://$STUB_DIR" \
    bash "$HERE/build-bd.sh" --from-release 2>&1)"
rel_rc=$?

if [ "$rel_rc" -eq 0 ]; then
    ok "--from-release exits 0 with correct naming and checksums"
else
    bad "--from-release exits 0 with correct naming and checksums" \
        "exit $rel_rc: $(printf '%s' "$rel_out" | tail -3)"
fi

[ -x "$FAKE_HOME/.local/bin/bd" ] \
    && ok "bd installed to fake home" \
    || bad "bd installed to fake home" "not found at $FAKE_HOME/.local/bin/bd"

# ============================================================================
echo ""
tl_summary
