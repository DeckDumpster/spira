#!/usr/bin/env bash
# tier: T1
# covers: testenv/Containerfile ../flake.nix ../flake.lock
#
# The testenv image installs its habit-hook tools from the same nixpkgs revision the flake pins.
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
ROOT="$(cd "$HERE/.." && pwd -P)"

echo "test-testenv-nixpkgs-pin.sh"

flake_rev="$(sed -n 's|.*inputs.nixpkgs.url = "github:NixOS/nixpkgs/\([0-9a-f]\{40\}\)".*|\1|p' "$ROOT/flake.nix")"
image_rev="$(sed -n 's|^ARG NIXPKGS_REV=\([0-9a-f]\{40\}\)$|\1|p' "$HERE/testenv/Containerfile")"
[ -n "$flake_rev" ] && ok "positive control: flake.nix pin found" || bad "no nixpkgs pin in flake.nix"
[ -n "$image_rev" ] && ok "positive control: Containerfile pin found" || bad "no NIXPKGS_REV in Containerfile"
is "Containerfile NIXPKGS_REV equals flake.nix nixpkgs" "$flake_rev" "$image_rev"
is "no cargo install of rust-code-analysis-cli in the image" "0" "$(grep -c 'cargo install.*rust-code-analysis' "$HERE/testenv/Containerfile")"
tl_summary
