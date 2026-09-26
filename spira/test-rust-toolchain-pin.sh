#!/usr/bin/env bash
# test-rust-toolchain-pin.sh — local cargo and CI build with the same compiler.
#
# rust-toolchain.toml pins the workspace toolchain; every workflow that installs Rust must
# install that same version. A drift lets a local cargo rewrite Cargo.lock to crates CI
# cannot build (PR 361, 2026-09-25: indexmap 2.14.2 needs edition2024, CI ran 1.82).
#
# tier: T0
# covers: rust-toolchain.toml .github/workflows/gate.yml .github/workflows/release.yml
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
    v="$(awk "/dtolnay\/rust-toolchain/{f=1} f&&/toolchain:/{gsub(/[' ]/,\"\",\$2); print \$2; exit}" FS=':' "$wf")"
    is "$(basename "$wf") installs the pinned toolchain" "$pin" "${v:-none}"
done
tl_summary
