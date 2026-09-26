#!/usr/bin/env bash
# test-rust-toolchain-pin.sh — local cargo and CI build with the same compiler.
#
# rust-toolchain.toml pins the workspace toolchain; every workflow that installs Rust must
# install that same version. A drift lets a local cargo rewrite Cargo.lock to crates CI
# cannot build (PR 361, 2026-09-25: indexmap 2.14.2 needs edition2024, CI ran 1.82).
#
# tier: T0
# covers: rust-toolchain.toml .github/workflows/gate.yml .github/workflows/release.yml spira/testenv/Containerfile
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"
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
    else
        is "$(basename "$wf") installs the pinned toolchain" "$pin" "$v"
    fi
done
# THE TEST IMAGE'S OWN COMPILER. A container whose pre-installed toolchain differs from
# the pin makes cargo fetch the pin into RUSTUP_HOME at test time, which is read-only for
# the runtime user — every Rust suite goes red inside the container while passing on the
# host, which has no such restriction.
img_rust="$(awk -F'[= ]+' '/^ARG RUST_VERSION=/{print $3; exit}' "$ROOT/testenv/Containerfile" 2>/dev/null)"
is "testenv Containerfile installs the pinned toolchain" "$pin" "$img_rust"

tl_summary
