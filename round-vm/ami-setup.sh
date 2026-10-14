#!/bin/bash
# Guest setup for the EC2 round image: what the Proxmox round template has beyond its
# base OS, then the template recipe itself. $1 tree dir, $2 toolchain, $3 CARGO_HOME.
# Hashed into the recipe (round-vm recipe-sha): editing it makes every older image stale.
set -euo pipefail
tree="$1" toolchain="$2" cache_home="$3"
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
apt-get install -y -qq podman git curl build-essential pkg-config libssl-dev openssh-server jq
curl -fsSL https://tailscale.com/install.sh | sh
systemctl enable --now ssh tailscaled
mkdir -p /root/.ssh && chmod 700 /root/.ssh
bash -s -- "$tree" "" "$toolchain" "$cache_home" < "$tree/round-vm/src/template.sh"
printf 'CARGO_HOME=%s\n' "$cache_home" > /etc/spira-round-ami.env
