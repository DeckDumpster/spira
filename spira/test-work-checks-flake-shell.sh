#!/usr/bin/env bash
# test-work-checks-flake-shell.sh — `work checks` runs a repository's rows inside its flake's
# dev shell: a tool the shell carries runs, one it does not fails naming the program.
#
# Real nix, real flake, real `work`; the fixture repo's shell carries jq and not uv. A tree
# with no flake.nix keeps the host PATH (the control row).
#
# tier: T1
# bare-host: needs the host's nix store and its flake input cache; skips when the shell cannot be realised
# covers: work/src/main.rs work/src/lib.rs repo-contract/src/* spira/conf.d/SPIRA_NIX_PROFILE_BIN
# priority: 2
# timeout: 300
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"

echo "test-work-checks-flake-shell.sh"

NIX_DIR="${SPIRA_TEST_NIX_PROFILE_BIN:-/nix/var/nix/profiles/default/bin}"
[ -x "$NIX_DIR/nix" ] || skip "no nix at $NIX_DIR"
WORK="$(command -v work)" || bail "work not on PATH"
NC="experimental-features = nix-command flakes"

REPO="$TMPDIR/repo"; PLAIN="$TMPDIR/plain"; RUN="$TMPDIR/run"
mkdir -p "$REPO/.spira" "$PLAIN/.spira" "$RUN"
cp "$HERE/../docs/spikes/repo-contract-phase-1-evidence/flake/flake.lock" "$REPO/flake.lock"
cat > "$REPO/flake.nix" <<'NIX'
{
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/7fd4004d0511991fff3a0524856a9be09e28c5f0";
  outputs = { self, nixpkgs }:
    let pkgs = import nixpkgs { system = "x86_64-linux"; };
    in { devShells.x86_64-linux.default = pkgs.mkShell { packages = [ pkgs.jq ]; }; };
}
NIX
for d in "$REPO" "$PLAIN"; do
    cat > "$d/.spira/checks.toml" <<'TOML'
[[check]]
name = "jq"
run = "jq --version"
budget_ms = 120000

[[check]]
name = "uv"
run = "uv --version"
budget_ms = 120000
TOML
    git -C "$d" init -q
    git -C "$d" add -A
    git -C "$d" -c user.email=t@t -c user.name=t commit -qm init
    git -C "$d" branch -f local/main HEAD
done

if ! env -i PATH=/usr/bin:/bin HOME="$HOME" NIX_CONFIG="$NC" "$NIX_DIR/nix" print-dev-env --json "path:$REPO" >/dev/null 2>&1; then
    skip "the fixture's dev shell cannot be realised here (no store path or no network)"
fi

CONF="$(tl_layer SPIRA_NIX_PROFILE_BIN="$NIX_DIR" SPIRA_RUN="$RUN")"
run_checks() {
    (cd "$1" && env -i PATH="/usr/bin:/bin" HOME="$HOME" NIX_CONFIG="$NC" \
        SPIRA_RELEASE="$SPIRA_RELEASE" SPIRA_TOML="$CONF" "$WORK" checks 2>&1)
}

out="$(run_checks "$REPO")"
want "flake tree: jq runs in the shell" "-- jq: ok" "$out"
want "flake tree: the row says it runs in the flake dev shell" "in: flake dev shell" "$out"
want "flake tree: uv, absent from the shell, fails" "-- uv: FAIL" "$out"
want "flake tree: the failure names the program" "uv" "$(grep -- '-- uv: FAIL' <<<"$out")"

out="$(run_checks "$PLAIN")"
case "$out" in *"flake dev shell"*) bad "no-flake tree: rows keep the host environment" "$out" ;; *) ok "no-flake tree: rows keep the host environment" ;; esac

tl_summary
