#!/bin/bash
#
# round-image-preflight-guest.sh — inside the throwaway base-OS container: run the image
# setup through the entry the Round AMI workflow uses. $1 toolchain.
# Like the real builds (Proxmox setup_command, EC2 ssm-commands.jq), it passes ami-setup.sh no
# foreign checkouts: a foreign repo's own dependency check is not the image's (sp-hyfjgl).
#
# Only the three things a container cannot do are replaced: systemd units, the nix daemon
# install and testenv's nested image build. Every cargo and rustup step runs for real.

# covers: spira/round-image-preflight.sh spira/round-image-preflight-guest.sh
set -euo pipefail
toolchain="$1"
export DEBIAN_FRONTEND=noninteractive
rm -rf /root/tree && mkdir -p /root/tree && cp -a /src/. /root/tree/

mkdir -p /etc/systemd/system /cache/toolchains /root/.cargo/rustup
ln -sfn /cache/registry /root/.cargo/registry
ln -sfn /cache/toolchains /root/.cargo/rustup/toolchains
mkdir -p /cache/toolchains/.rustup-tmp && ln -sfn /cache/toolchains/.rustup-tmp /root/.cargo/rustup/tmp
export CARGO_TARGET_DIR=/cache/target
# testenv finds its harness above its own binary, and the cached target is not under the tree
# (sp-0aqtny). The real template builds into the tree's own target, so template.sh needs none.
export SPIRA_TESTENV_HARNESS=/root/tree

printf '#!/bin/sh\nexit 0\n' > /usr/local/bin/systemctl
cat > /usr/local/bin/podman <<'SHIM'
#!/bin/sh
case "$1 $2" in
    "image exists") exit 0 ;;
    "images "*|"images ") exit 0 ;;
esac
exec /usr/bin/podman "$@"
SHIM
chmod +x /usr/local/bin/systemctl /usr/local/bin/podman

nix_bin=/nix/var/nix/profiles/default/bin
mkdir -p "$nix_bin"
cat > "$nix_bin/nix" <<'SHIM'
#!/bin/sh
out=""
while [ $# -gt 0 ]; do [ "$1" = -o ] && out="$2"; shift; done
store=/nix-preflight-env; mkdir -p "$store/bin"
printf '#!/bin/sh\n[ "$1" = --stop-server ] && exit 0\nexec "$@"\n' > "$store/bin/sccache"
for t in tailscale tailscaled node npm npx; do printf '#!/bin/sh\nexit 0\n' > "$store/bin/$t"; done
chmod +x "$store"/bin/*
[ -z "$out" ] || ln -sfn "$store" "$out"
SHIM
chmod +x "$nix_bin/nix"

cd /root/tree
bash -x round-vm/ami-setup.sh /root/tree "$toolchain" /root/.cargo

# Names the recipe's cargo commands pass to -p must each resolve to exactly one package.
export PATH="/root/.cargo/bin:$PATH"
cd /src
for n in $(cat round-vm/src/template.sh .github/workflows/round-ami-build.yml | grep -o -- 'cargo [a-z]* [^|]*' | grep -o -- '-p [A-Za-z0-9_-]*' | cut -d' ' -f2 | sort -u); do
    set -x; cargo pkgid --locked -p "$n" >/dev/null; { set +x; } 2>/dev/null
done
