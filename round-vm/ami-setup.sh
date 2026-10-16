#!/bin/bash
# Guest setup for the EC2 round image: what the Proxmox round template has beyond its
# base OS, then the template recipe itself. $1 tree dir, $2 toolchain, $3 CARGO_HOME.
# Hashed into the recipe (round-vm recipe-sha): editing it makes every older image stale.
set -euo pipefail
tree="$1" toolchain="$2" cache_home="$3"
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
apt-get install -y -qq podman git curl build-essential pkg-config libssl-dev openssh-server jq
systemctl enable --now ssh
mkdir -p /root/.ssh && chmod 700 /root/.ssh
bash -s -- "$tree" "" "$toolchain" "$cache_home" < "$tree/round-vm/src/template.sh"
env_bin="$(readlink -f "${HOME:-/root}/spira-env")/bin"
ln -sfn "$env_bin/tailscale" /usr/local/bin/tailscale
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
