#!/usr/bin/env bash
#
# test-testenv-image-tag.sh — _image_tag() hashes the build closure, not just the Containerfile.
#
# WHAT THIS TESTS
# ---------------
# 1. DEPENDENCY MANIFEST: editing deps.toml changes the tag.
# 2. BD PIN: editing the bd pin file changes the tag.
# 3. UNRELATED FILE: a file outside the closure does not change the tag.
# 4. POSITIVE CONTROL: with the closure narrowed back to the Containerfile only (the old
#    behaviour), a deps.toml edit does NOT change the tag — proving that the widening in
#    the fixed _image_tag() is what produces the correct result above.
#
# FIXTURES
# --------
# Each call to testenv.sh tag runs inside a scratch copy of the spira/ directory, so
# edits to fixture files do not touch the working tree. testenv.sh derives HERE from its
# own BASH_SOURCE[0], so running the copy makes it look at the fixture's conf.sh and
# Containerfile rather than the source originals.
# SPIRA_BD_PIN is set in the environment; conf.sh records env-set keys before reading any
# config file and skips them, so the fixture path is never overridden.
#
# SKIP CONDITION: none — only sha256sum is required.
#
# defect: sp-2v7v
# tier: T1
# covers: spira/testenv.sh spira/conf.sh spira/deps.toml
# covers: spira/testenv/Containerfile
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"
differs() { [ "$2" != "$3" ] && ok "$1" \
            || bad "$1" "tag did not change: was and still is [$2]"; }
same()    { [ "$2" = "$3" ] && ok "$1" \
            || bad "$1" "tag changed unexpectedly: [$2] → [$3]"; }

echo "test-testenv-image-tag.sh"

# ──────────────────────────────────────────────────────────────────────────────
# Fixture setup: scratch copy of spira/ so edits stay local.
# ──────────────────────────────────────────────────────────────────────────────
TMP="$(mktemp -d)"
FIXTURE="$TMP/spira"
mkdir -p "$FIXTURE/testenv"
cp "$HERE/testenv.sh"             "$FIXTURE/testenv.sh"
cp "$HERE/conf.sh"                "$FIXTURE/conf.sh"
cp "$HERE/deps.toml"              "$FIXTURE/deps.toml"
cp "$HERE/testenv/Containerfile"  "$FIXTURE/testenv/Containerfile"

# Fixture pin file. conf.sh will skip it because SPIRA_BD_PIN is already in the
# environment when conf.sh is sourced (env-set keys win over config and defaults).
PIN="$TMP/bd-pin"
printf 'BD_PIN_MIGRATIONS=42\nBD_PIN_SHA256=aabbcc\n' > "$PIN"

cleanup() { rm -rf "$TMP"; }
trap cleanup EXIT INT TERM

# Call testenv.sh tag from the fixture copy, pinning the bd-pin path via env.
get_tag() {
    SPIRA_BD_PIN="$PIN" bash "$FIXTURE/testenv.sh" tag 2>/dev/null
}

# ──────────────────────────────────────────────────────────────────────────────
echo
echo "baseline:"
# ──────────────────────────────────────────────────────────────────────────────
tag_base=$(get_tag)
[ -n "$tag_base" ] \
    && ok "tag is non-empty ($tag_base)" \
    || bad "tag" "empty output — testenv.sh tag subcommand missing or failing"

# ──────────────────────────────────────────────────────────────────────────────
echo
echo "dependency manifest → tag changes:"
# ──────────────────────────────────────────────────────────────────────────────
# Add a harmless program entry to deps.toml. _image_tag() hashes the manifest file, so
# the hash must change when it changes.
printf '\n[[dep]]\nname = "testprog"\ntier = "optional"\n' >> "$FIXTURE/deps.toml"
tag_manifest=$(get_tag)
differs "manifest edit changes tag" "$tag_base" "$tag_manifest"
cp "$HERE/deps.toml" "$FIXTURE/deps.toml"   # restore

# ──────────────────────────────────────────────────────────────────────────────
echo
echo "bd pin → tag changes:"
# ──────────────────────────────────────────────────────────────────────────────
printf 'BD_PIN_MIGRATIONS=99\nBD_PIN_SHA256=ddee00\n' > "$PIN"
tag_pin=$(get_tag)
differs "bd pin edit changes tag" "$tag_base" "$tag_pin"
printf 'BD_PIN_MIGRATIONS=42\nBD_PIN_SHA256=aabbcc\n' > "$PIN"   # restore

# ──────────────────────────────────────────────────────────────────────────────
echo
echo "unrelated file → tag unchanged:"
# ──────────────────────────────────────────────────────────────────────────────
# A file outside the closure must not affect the hash.
printf 'noise\n' > "$FIXTURE/unrelated.txt"
tag_unrelated=$(get_tag)
same "unrelated file does not change tag" "$tag_base" "$tag_unrelated"
rm "$FIXTURE/unrelated.txt"

# ──────────────────────────────────────────────────────────────────────────────
echo
echo "positive control — narrow closure (Containerfile only) ignores conf.sh:"
# ──────────────────────────────────────────────────────────────────────────────
# The old _image_tag() was:  sha256sum "$TESTENV_DIR/Containerfile" | cut -c1-12
# With that narrow closure, editing deps.toml must NOT change the tag.
# This proves that it is the closure widening — not some other difference — that
# makes the three tests above pass.
narrow_hash() {
    sha256sum "$FIXTURE/testenv/Containerfile" 2>/dev/null | cut -c1-12
}
narrow_base=$(narrow_hash)
printf '\n[[dep]]\nname = "testprog"\ntier = "optional"\n' >> "$FIXTURE/deps.toml"
narrow_after=$(narrow_hash)
same "positive control: narrow closure unchanged by deps.toml edit" "$narrow_base" "$narrow_after"
cp "$HERE/deps.toml" "$FIXTURE/deps.toml"   # restore

# ──────────────────────────────────────────────────────────────────────────────
echo
echo "the tag is a property of the content, not of where the checkout sits:"
# ──────────────────────────────────────────────────────────────────────────────
# `sha256sum FILE` prints "<hash>  <path>", and hashing that output puts the path
# into the tag. Two checkouts of the same commit then compute different tags,
# which is silent and expensive in both directions: every worktree rebuilds its
# own 1.8 GB image, and an image published from one checkout can never be pulled
# by another. Nothing reports a fault — the build simply always runs.
OTHER="$TMP/elsewhere/spira"
mkdir -p "$OTHER/testenv"
cp "$FIXTURE/testenv.sh"            "$OTHER/testenv.sh"
cp "$FIXTURE/conf.sh"               "$OTHER/conf.sh"
cp "$FIXTURE/deps.toml"             "$OTHER/deps.toml"
cp "$FIXTURE/testenv/Containerfile" "$OTHER/testenv/Containerfile"
tag_elsewhere="$(SPIRA_BD_PIN="$PIN" bash "$OTHER/testenv.sh" tag 2>/dev/null)"
same "same content at another path gives the same tag" "$tag_base" "$tag_elsewhere"

# ──────────────────────────────────────────────────────────────────────────────
echo
tl_summary
