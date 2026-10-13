set -euo pipefail
: "${USER:=$(id -un)}"
: "${HOME:=$(getent passwd "$(id -u)" | cut -d: -f6)}"
export HOME USER
work="$1" host_addr="$2" toolchain="$3" cache_home="$4"
cd "$work"
export CARGO_HOME="${cache_home:-$HOME/.cargo}"
mkdir -p "$CARGO_HOME/bin"
if [ -n "$toolchain" ] && [ ! -x "$CARGO_HOME/bin/rustc" ]; then
    echo "round-vm template: installing rust $toolchain standalone into $CARGO_HOME (must match the host's rustc byte-for-byte, sp-xjnzl)" >&2
    tmp="$(mktemp -d)"
    curl -fsSL "https://static.rust-lang.org/dist/rust-${toolchain}-x86_64-unknown-linux-gnu.tar.gz" -o "$tmp/rust.tar.gz"
    tar -xzf "$tmp/rust.tar.gz" -C "$tmp" --strip-components=1
    "$tmp/install.sh" --prefix="$CARGO_HOME" --destdir="" --disable-ldconfig >&2
    rm -rf "$tmp"
fi
export PATH="$CARGO_HOME/bin:$PATH"
export SPIRA_HOME="$work/spira"
mkdir -p "$work/.runtime/spira"
cat > "$work/template-config.toml" <<TEMPLATECFG
[spira]
run = "$work/.runtime/spira"
releases = "$work/.runtime/releases"
home_repo = "$work"
TEMPLATECFG
export SPIRA_TOML="$work/spira-config/tests/fixtures/complete.toml:$work/template-config.toml"
nix_bin=/nix/var/nix/profiles/default/bin
if [ ! -x "$nix_bin/nix" ]; then
    echo "round-vm template: installing nix (multi-user)" >&2
    curl -fsSL https://nixos.org/nix/install | sh -s -- --daemon --yes >&2
fi
export PATH="$nix_bin:$PATH"
echo "round-vm template: building spira-env from this tree's flake.lock" >&2
nix --extra-experimental-features 'nix-command flakes' build "path:$work#spira-env" -o "$HOME/spira-env" >&2
ln -sfn "$(readlink -f "$HOME/spira-env")/bin/sccache" "$CARGO_HOME/bin/sccache"
export RUSTC_WRAPPER="$CARGO_HOME/bin/sccache"
export SCCACHE_IGNORE_SERVER_IO_ERROR=1
if [ -n "$host_addr" ]; then
    export SCCACHE_WEBDAV_ENDPOINT="http://${host_addr}:9431"
    export SCCACHE_WEBDAV_KEY_PREFIX="/"
fi
# testenv's own cold-build step stages a sibling `spira-config` binary into the image build
# context for the Containerfile's doctor-check (container.rs's stage_spira_config,
# testenv/Containerfile) — `cargo run -p testenv` alone never builds spira-config, since
# nothing else in the workspace pulls in ITS bin target. Built explicitly here so the
# sibling exists before testenv ever looks for it.
cargo build -q --locked --release -p testenv -p spira-config
img="$(cargo run -q --locked --release -p testenv -- container image)"
if [ -z "$img" ]; then echo "round-vm template: testenv container image printed no image ref" >&2; exit 1; fi
podman images --format '{{.Repository}}:{{.Tag}}' \
    | { grep '^localhost/spira-testenv:' || true; } | { grep -vxF "$img" || true; } \
    | while read -r old; do podman rmi "$old" >&2; done
cargo fetch --locked >&2
"$CARGO_HOME/bin/sccache" --stop-server >&2 || true
cd /
rm -rf "$work" /root/.ssh/authorized_keys
sync
printf 'image=%s\n' "$img"
