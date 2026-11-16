#!/bin/bash
# Usage: reach.sh <flake-dir>. Prints each command before it runs, with its exit status.
F=$(readlink -f "$1"); N=/nix/var/nix/profiles/default/bin/nix; REL=${SPIRA_RELEASE:?}
GP="$REL/bin:$REL/spira:/usr/local/bin:/usr/bin:/bin:$HOME/.local/bin:$HOME/.cargo/bin"
GP2="$GP:/nix/var/nix/profiles/default/bin"
IMG=localhost/spira-testenv:93400812ae94
NC="experimental-features = nix-command flakes"
run() { echo "\$ $*"; "$@" 2>&1 | tail -${TAILN:-4}; echo "[exit ${PIPESTATUS[0]}]"; }
echo "== builder: PATH=$PATH"
run bash -c 'command -v nix'
run $N eval --expr 1+1
echo "== gate env -i, PATH as engine.rs builds it (release bin:spira:system dirs:spira.path tail)"
run env -i HOME=$HOME PATH=$GP bash -c 'command -v nix'
echo "== gate env -i, tail extended with the nix profile bin"
run env -i HOME=$HOME PATH=$GP2 nix eval --expr 1+1
run env -i PATH=$GP2 nix develop $F --command uv --version
run env -i HOME=/nonexistent PATH=$GP2 nix develop $F --command uv --version
run env -i HOME=$HOME XDG_CACHE_HOME=/tmp/xc-$$ PATH=$GP2 nix develop $F --command uv --version
echo "== testenv container (network as testenv ISOLATED_NETWORK)"
P=(podman run --rm --network pasta:-T,none,--no-map-gw --security-opt apparmor=unconfined -e "NIX_CONFIG=$NC")
run "${P[@]}" $IMG sh -c 'command -v nix'
run "${P[@]}" -v /nix:/nix:ro $IMG $N eval --expr 1+1
run "${P[@]}" -v /nix:/nix:ro $IMG $N eval --expr 1+1
run podman run --rm --network pasta:-T,none,--no-map-gw $IMG $N eval --expr 1+1
run "${P[@]}" -v /nix:/nix:ro -v $F:/flake:ro $IMG sh -c "cd /tmp && $N print-dev-env --json path:/flake > /tmp/e.json; rc=\$?; echo print-dev-env rc=\$rc bytes=\$(wc -c < /tmp/e.json)"
run "${P[@]}" -v /nix:/nix:ro -v $F:/flake:ro $IMG sh -c "cd /tmp && $N develop path:/flake --command uv --version"
echo "== acceptance command, from the worktree root"
cd "$(git -C "$F" rev-parse --show-toplevel)" && run $N develop ./docs/spikes/repo-contract-phase-1-evidence/flake --command uv --version
