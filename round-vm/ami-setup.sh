#!/bin/bash
# Guest setup for the EC2 round image: what the Proxmox round template has beyond its
# base OS, then the template recipe itself. $1 tree dir, $2 toolchain, $3 CARGO_HOME,
# $4 optional space-separated checkouts of registered repos: their deploy/runner-deps.sh
# declares the nextest pin and must pass --check on the finished image.
# Hashed into the recipe (round-vm recipe-sha): editing it makes every older image stale.
set -euo pipefail
tree="$1" toolchain="$2" cache_home="${3:-${HOME:-/root}/.cargo}" foreign="${4:-}"
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
apt-get install -y -qq podman git curl build-essential pkg-config libssl-dev openssh-server jq sqlite3 xz-utils python3 make
systemctl enable --now ssh
mkdir -p /root/.ssh && chmod 700 /root/.ssh
bash -s -- "$tree" "${SPIRA_TEMPLATE_HOST_ADDR:-}" "$toolchain" "$cache_home" < "$tree/round-vm/src/template.sh"
# template.sh removes its work dir, which may be ours: cargo install needs a cwd that exists.
cd /
env_bin="$(readlink -f "${HOME:-/root}/spira-env")/bin"
for t in tailscale node npm npx; do ln -sfn "$env_bin/$t" "/usr/local/bin/$t"; done
nextest=0.9.148
for d in $foreign; do
    v="$(sed -n 's/^NEXTEST_VERSION=//p' "$d/deploy/runner-deps.sh" | head -1)"
    [ -z "$v" ] || nextest="$v"
done
export PATH="$cache_home/bin:$PATH"
export CARGO_HOME="$cache_home" RUSTUP_HOME="$cache_home/rustup"
cargo install --locked cargo-nextest --version "$nextest"
ln -sfn "$cache_home/bin/cargo-nextest" /usr/local/bin/cargo-nextest
llvm_cov=0.6.16
cargo install --locked cargo-llvm-cov --version "$llvm_cov"
ln -sfn "$cache_home/bin/cargo-llvm-cov" /usr/local/bin/cargo-llvm-cov
rustup component add llvm-tools --toolchain "$toolchain"
for d in $foreign; do bash "$d/deploy/runner-deps.sh" --check; done
cat > /etc/systemd/system/tailscaled.service <<UNIT
[Unit]
Description=Tailscale node agent
After=network-online.target
Wants=network-online.target

[Service]
ExecStart=$env_bin/tailscaled --state=/var/lib/tailscale/tailscaled.state --socket=/run/tailscale/tailscaled.sock
RuntimeDirectory=tailscale
StateDirectory=tailscale
Restart=on-failure

[Install]
WantedBy=multi-user.target
UNIT
systemctl daemon-reload
systemctl enable --now tailscaled
printf 'CARGO_HOME=%s\n' "$cache_home" > /etc/spira-round-ami.env
[ -z "${SPIRA_RECIPE_SHA:-}" ] || printf '%s\n' "$SPIRA_RECIPE_SHA" > /etc/spira-round-recipe-sha
