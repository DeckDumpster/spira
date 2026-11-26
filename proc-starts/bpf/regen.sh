#!/bin/sh
# Rebuild the committed bpf object from its source with the flake's clang (llvmPackages.clang-unwrapped).
set -eu
cd "$(dirname "$0")"
command -v clang >/dev/null || { echo "clang not found on PATH: add llvmPackages.clang-unwrapped from flake.nix" >&2; exit 1; }
clang -target bpf -O2 -g -fdebug-prefix-map="$(pwd)"=. -c proc_starts.bpf.c -o proc_starts.bpf.o
sha256sum proc_starts.bpf.c | cut -d' ' -f1 > proc_starts.bpf.c.sha256
