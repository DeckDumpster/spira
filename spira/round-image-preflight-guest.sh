#!/bin/bash
#
# round-image-preflight-guest.sh — inside the throwaway base-OS container: run the image
# setup through the entry the Round AMI workflow uses. $1 toolchain, $2 foreign checkouts.
#
# Only the three things a container cannot do are replaced: systemd units, the nix daemon
# install and testenv's nested image build. Every cargo and rustup step runs for real.

# covers: spira/round-image-preflight.sh spira/round-image-preflight-guest.sh
set -euo pipefail
toolchain="$1" foreign="${2:-}"
export DEBIAN_FRONTEND=noninteractive
rm -rf /root/tree && mkdir -p /root/tree && cp -a /src/. /root/tree/

mkdir -p /etc/systemd/system /cache/toolchains /root/.cargo/rustup
ln -sfn /cache/registry /root/.cargo/registry
ln -sfn /cache/toolchains /root/.cargo/rustup/toolchains
export CARGO_TARGET_DIR=/cache/target

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
bash -x round-vm/ami-setup.sh /root/tree "$toolchain" /root/.cargo "$foreign"

# Names the recipe's cargo commands pass to -p must each resolve to exactly one package.
export PATH="/root/.cargo/bin:$PATH"
cd /src
for n in $(cat round-vm/src/template.sh .github/workflows/round-ami-build.yml | grep -o -- 'cargo [a-z]* [^|]*' | grep -o -- '-p [A-Za-z0-9_-]*' | cut -d' ' -f2 | sort -u); do
    set -x; cargo pkgid --locked -p "$n" >/dev/null; { set +x; } 2>/dev/null
done
