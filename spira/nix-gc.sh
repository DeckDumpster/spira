#!/usr/bin/env bash
# nix-gc.sh — collects unreferenced Nix store paths, keeping the current and the previous
# spira-env: install roots the current one with `nix build -o` and the replaced one at
# spira-env.prev, so everything else unreferenced is collectable.
# covers: systemd/spira-nix-gc.service systemd/spira-nix-gc.timer install/src/bin/install.rs
set -uo pipefail

nix_gc_main() {
    local env_link="${SPIRA_NIX_ENV_LINK:-$HOME/.local/lib/spira-env}"
    if [ ! -d "$env_link/bin" ]; then
        echo "nix-gc: $env_link does not resolve to a spira-env — refusing to collect" >&2
        return 1
    fi
    if [ -L "$env_link.prev" ] && [ ! -d "$env_link.prev/bin" ]; then
        echo "nix-gc: $env_link.prev is dangling — refusing to collect" >&2
        return 1
    fi
    local profile_bin="${SPIRA_NIX_PROFILE_BIN:-/nix/var/nix/profiles/default/bin}" gc="" dir
    for dir in "$profile_bin" "$env_link/bin"; do
        if [ -x "$dir/nix-collect-garbage" ]; then gc="$dir/nix-collect-garbage"; break; fi
    done
    if [ -z "$gc" ]; then
        echo "nix-gc: nix-collect-garbage not found in $profile_bin or $env_link/bin" >&2
        return 1
    fi
    "$gc"
}

if [ "${BASH_SOURCE[0]}" = "$0" ]; then
    nix_gc_main "$@"
fi
