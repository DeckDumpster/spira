set -euo pipefail
: "${USER:=$(id -un)}"
: "${HOME:=$(getent passwd "$(id -u)" | cut -d: -f6)}"
export HOME USER
work="$1" host_addr="$2" toolchain="$3" cache_home="$4"
cd "$work"
export CARGO_HOME="${cache_home:-$HOME/.cargo}"
mkdir -p "$CARGO_HOME/bin"
# toolchain-begin
have=""
[ -x "$CARGO_HOME/bin/rustc" ] && have="$("$CARGO_HOME/bin/rustc" --version 2>/dev/null | cut -d' ' -f2)"
if [ -n "$toolchain" ] && [ "$have" != "$toolchain" ]; then
    echo "round toolchain $toolchain (installed at round time; the image has ${have:-none}); must match the host's rustc byte-for-byte, sp-xjnzl" >&2
    command -v rustup >/dev/null 2>&1 || apt-get install -y -qq rustup >&2
    export RUSTUP_HOME="$CARGO_HOME/rustup"
    rustup toolchain install "$toolchain" --profile minimal --no-self-update >&2
    for b in "$RUSTUP_HOME"/toolchains/"$toolchain"-*/bin/*; do ln -sfn "$b" "$CARGO_HOME/bin/${b##*/}"; done
fi
# toolchain-end
# default-begin
if [ -n "$toolchain" ]; then
    (
        export RUSTUP_HOME="${HOME}/.rustup"
        rustup toolchain install "$toolchain" --profile minimal --no-self-update >&2
        rustup default "$toolchain" >&2
    )
fi
# default-end
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
    nix_installer="$(mktemp -d)/install"
    curl -fsSL -o "$nix_installer" https://releases.nixos.org/nix/nix-2.28.3/install
    echo "46b8d7165dceb471f4346366b3a93f1009407b99729b843b8664918f4cc800a0  $nix_installer" | sha256sum -c - >&2
    sh "$nix_installer" --daemon --yes >&2
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
cargo fetch --locked --target x86_64-unknown-linux-gnu >&2
"$CARGO_HOME/bin/sccache" --stop-server >&2 || true
# verify-begin
if ! plain="$(env -i PATH="${PLAIN_PATH:-/usr/local/bin:/usr/bin:/bin}" HOME="$HOME" cargo --version 2>&1)"; then
    echo "round-vm template: plain cargo does not run on this clone, so a round would go red with no output: $plain" >&2
    exit 1
fi
# verify-end
cd /
rm -rf "$work" /root/.ssh/authorized_keys
sync
printf 'image=%s\n' "$img"
