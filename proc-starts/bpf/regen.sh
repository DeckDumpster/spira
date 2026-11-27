#!/bin/sh
# Rebuild the committed bpf object from its source with the flake's clang (llvmPackages.clang-unwrapped).
set -eu
cd "$(dirname "$0")"
command -v clang >/dev/null || { echo "clang not found on PATH: add llvmPackages.clang-unwrapped from flake.nix" >&2; exit 1; }
# Unexpanded until eval, so the manifest never records the build directory.
flags='-target bpf -O2 -g -fdebug-prefix-map="$(pwd)"=.'
eval "clang $flags -c proc_starts.bpf.c -o proc_starts.bpf.o"
sha() { sha256sum "$1" | cut -d' ' -f1; }
cat > proc_starts.bpf.manifest <<MANIFEST
source_sha256=$(sha proc_starts.bpf.c)
object_sha256=$(sha proc_starts.bpf.o)
clang=$(clang --version | head -n1)
flags=$flags
MANIFEST
