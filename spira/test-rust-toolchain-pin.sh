#!/usr/bin/env bash
# test-rust-toolchain-pin.sh — local cargo and CI build with the same compiler.
#
# rust-toolchain.toml pins the workspace toolchain; every workflow that installs Rust must
# install that same version. A drift lets a local cargo rewrite Cargo.lock to crates CI
# cannot build (PR 361, 2026-09-25: indexmap 2.14.2 needs edition2024, CI ran 1.82).
#
# tier: T0
# covers: rust-toolchain.toml .github/workflows/* spira/testenv/Containerfile round-vm/* batcher-cut/* testenv/* spira/acceptance-local.sh flake.nix
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"
tl_config SPIRA_TEST_CASE_DEADLINE=90   # setup builds real git/dolt fixtures; a loaded VM doubles it
ROOT="$(cd "$HERE/.." && pwd)"

pin="$(awk -F'"' '/^channel/{print $2; exit}' "$ROOT/rust-toolchain.toml" 2>/dev/null)"
want "rust-toolchain.toml pins a channel" "1." "${pin:-none}"

# POSITIVE CONTROL: the extractor finds a planted mismatch.
ctl="$(printf "        uses: dtolnay/rust-toolchain@master\n        with:\n          toolchain: '9.99.0'\n" \
    | awk "/dtolnay\/rust-toolchain/{f=1} f&&/toolchain:/{gsub(/[' ]/,\"\",\$2); print \$2; exit}" FS=':')"
is "positive control: the extractor reads a planted version" "9.99.0" "$ctl"

for wf in "$ROOT"/.github/workflows/*.yml; do
    grep -q 'dtolnay/rust-toolchain' "$wf" || continue
    # Look only at the lines belonging to the dtolnay/rust-toolchain step itself: an
    # explicit `toolchain:` there can drift from the pin, but an omitted one defers to
    # rust-toolchain.toml at run time (dtolnay/rust-toolchain's own behavior) and cannot.
    block="$(awk '/dtolnay\/rust-toolchain/{f=1; n=0} f{print; n++} f&&n>=4{exit}' "$wf")"
    v="$(printf '%s\n' "$block" | awk -F':' '/toolchain:/{gsub(/[\x27 ]/,"",$2); print $2; exit}')"
    if [ -z "$v" ]; then
        ok "$(basename "$wf") defers to rust-toolchain.toml (no explicit toolchain)"
    elif [ "$v" = '${{steps.toolchain.outputs.version}}' ]; then
        # Read from rust-toolchain.toml by the workflow's own step, so it follows the pin
        # by construction; the step itself must be reading that file.
        want "$(basename "$wf") reads its toolchain from rust-toolchain.toml" \
            "rust-toolchain.toml" "$(grep -A4 'id: toolchain' "$wf")"
    else
        is "$(basename "$wf") installs the pinned toolchain" "$pin" "$v"
    fi
done
# THE TEST IMAGE'S OWN COMPILER. It is a build argument testenv fills from rust-toolchain.toml;
# a default here would be a second pin that wins whenever the argument is forgotten.
if grep -Eq '^ARG RUST_VERSION=' "$ROOT/spira/testenv/Containerfile"; then
    bad "testenv Containerfile takes RUST_VERSION from testenv, not a default" "ARG RUST_VERSION has a default"
else
    ok "testenv Containerfile takes RUST_VERSION from testenv, not a default"
fi

# ONE PIN, ONE SOURCE. No tracked file but rust-toolchain.toml may name a concrete Rust or
# Node version as a toolchain pin; test sources and fixtures are inputs, not pins. Node's
# one source is flake.nix (unversioned nodejs follows flake.lock).
PIN_RE='channel *= *"?[0-9]+\.[0-9]+|RUST_VERSION[= ]+[0-9]+\.|RUSTUP_TOOLCHAIN=[0-9]|toolchain (install|default) [0-9]+\.[0-9]+|toolchain: *.?[0-9]+\.[0-9]+|--toolchain[ =]+.?[0-9]+\.[0-9]|rust:[0-9]+\.[0-9]|nodejs_[0-9]+|node-version|"node": *"|\bnode:[0-9]+'
EXEMPT_RE='^(rust-toolchain\.toml|spira/test-[^/]*\.sh|round-vm/ami-setup\.sh|.*/tests?\.rs|.*/tests/.*)$'
second_pins() {
    local dir="$1" f
    while IFS= read -r f; do
        [[ "$f" =~ $EXEMPT_RE ]] && continue
        [ -f "$dir/$f" ] || continue
        grep -InE "$PIN_RE" "$dir/$f" 2>/dev/null | sed "s#^#$f:#"
    done
}
# round-vm/ami-setup.sh pins nextest's own compiler (an install-time tool, not the workspace
# toolchain) and is exempt by name.
fx="$(mktemp -d)"; trap 'rm -rf "$fx"' EXIT
mkdir -p "$fx/ci"
printf '[toolchain]\nchannel = "9.99.0"\n' > "$fx/rust-toolchain.toml"
printf 'ARG RUST_VERSION=8.88.0\n' > "$fx/ci/image"
printf 'node-version: 20\n' > "$fx/ci/node"
planted="$(printf 'rust-toolchain.toml\nci/image\nci/node\n' | second_pins "$fx")"
want "positive control: a second Rust pin is found" "ci/image:1:ARG RUST_VERSION=8.88.0" "$planted"
want "positive control: a Node pin is found" "ci/node:1:node-version: 20" "$planted"
nofile="$(printf 'rust-toolchain.toml\n' | second_pins "$fx")"
is "the pin file itself is never an offender" "" "$nofile"
real="$(git -C "$ROOT" ls-files 2>/dev/null | second_pins "$ROOT")"
is "no tracked file names a second Rust or Node pin" "" "$real"

tl_summary
