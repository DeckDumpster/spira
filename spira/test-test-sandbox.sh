#!/usr/bin/env bash
#
# test-test-sandbox.sh — cargo's target runner, tools/test-sandbox/runner, runs a test binary
# where it cannot see the caller's installation, and never runs one unsandboxed.
#
#   ./test-test-sandbox.sh
#
# A SUITE, NOT A RUST TEST. What is proved is what a test binary can reach once the runner has
# started it. A Rust test is itself started through the runner, so it would have to nest a
# second sandbox inside the first, and a kernel that confines bwrap's children refuses that.
#
# THE PROBE IS A REAL TEST BINARY, a libtest harness compiled here, started through the runner
# exactly as cargo starts one. The caller's home is a fixture that holds an installation (its
# config directory, its run directory, its bin directory), so the suite proves the probe could have found
# them — the planted config is read back from outside first (law-absence-needs-a-positive-control)
# — and the real home is never touched.
#
# tier: T1
# covers: tools/test-sandbox/* .cargo/config.toml testenv/src/sandbox.rs
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

RUNNER="$(cd "$HERE/.." && pwd -P)/tools/test-sandbox/runner"
WORKSPACE="$(cd "$HERE/.." && pwd -P)"
command -v bwrap >/dev/null 2>&1 || bail "bwrap is not installed; the runner cannot work here and the suite proves nothing"
command -v rustc >/dev/null 2>&1 || bail "rustc is not on PATH; the probe cannot be built"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

home="$TMP/home"
mkdir -p "$home/.config/spira" "$home/spira/run" "$home/.local/bin"
echo 'planted = true' > "$home/.config/spira/planted"
printf '#!/bin/sh\n' > "$home/.local/bin/work"
chmod +x "$home/.local/bin/work"

# Outside the sandbox the installation is there to be found.
is "the planted config reads back from outside" "planted = true" "$(cat "$home/.config/spira/planted")"

cat > "$TMP/probe.rs" <<'EOF'
use std::fs;
use std::path::PathBuf;

#[test]
fn probe() {
    println!(); // libtest's own "test probe ..." may share the first line under --nocapture
    let home = PathBuf::from(std::env::var("HOME").unwrap_or_default());
    let read = fs::read_to_string(home.join(".config/spira/planted"));
    println!("READ_CONFIG {:?}", read.map(|s| s.trim().to_string()));
    println!("CONFIG_DIR {:?}", fs::read_dir(home.join(".config/spira")).map(|d| d.count()));
    let write = fs::create_dir_all(home.join("spira/run")).and_then(|()| fs::write(home.join("spira/run/x"), "escaped\n"));
    println!("WRITE_RUN {write:?}");
    println!("LOCAL_BIN {:?}", fs::read_dir(home.join(".local/bin")).map(|d| d.count()));
    println!("WRITE_TMP {:?}", fs::write(std::env::temp_dir().join("probe-scratch"), "ok"));
    let mut keys: Vec<String> = std::env::vars().map(|(k, _)| k).filter(|k| k.starts_with("SPIRA")).collect();
    keys.sort();
    println!("SPIRA_KEYS {}", keys.join(","));
    println!("SPIRA_HOME {}", std::env::var("SPIRA_HOME").unwrap_or_default());
    let ifs = fs::read_to_string("/proc/net/dev").unwrap_or_default();
    let ifs: Vec<&str> = ifs.lines().skip(2).filter_map(|l| l.split(':').next()).map(str::trim).filter(|i| *i != "lo").collect();
    println!("NET_IFS {}", ifs.join(","));
    let git = std::env::var("GIT_CONFIG_SYSTEM").map(fs::read_to_string);
    println!("GIT_IDENTITY {git:?}");
}
EOF
mkdir -p "$TMP/target/debug/deps"
rustc --edition 2021 --test -o "$TMP/target/debug/deps/probe-0" "$TMP/probe.rs" >"$TMP/rustc.out" 2>&1 ||
    bail "the probe did not compile: $(cat "$TMP/rustc.out")"
probe=("$TMP/target/debug/deps/probe-0" --exact probe --nocapture --test-threads=1)

# The caller's own environment carries the live install's keys; none of them may get in.
run() { env -i HOME="$home" PATH="/usr/bin:/bin" SPIRA_HOME=/the/installed/harness SPIRA_RUN="$home/spira/run" "$@"; }
field() { sed -n "s/^$1 //p" <<<"$2"; }
# An exit status alone names nothing: a red one carries what the run printed (bwrap's own
# refusal is on its stderr).
# Each line of it stays a TAP comment.
rcis() { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted rc=$2 got rc=$3; the run printed:"$'\n'"${4//$'\n'/$'\n'# }"; }

# Run bare first: the probe's own checks find the installation when nothing hides it.
out="$(run "${probe[@]}" 2>&1)"
want "bare, the probe reads the caller's config" 'Ok("planted = true")' "$(field READ_CONFIG "$out")"
want "bare, the probe writes the install's run directory" "Ok(())" "$(field WRITE_RUN "$out")"
want "bare, the probe sees the caller's SPIRA_ keys" "SPIRA_RUN" "$(field SPIRA_KEYS "$out")"
[ -n "$(field NET_IFS "$out")" ] && ok "bare, the probe sees a network interface besides loopback" ||
    bad "bare, the probe sees a network interface besides loopback" "$out"
rm -f "$home/spira/run/x"

out="$(run "$RUNNER" "${probe[@]}" 2>&1)"
rc=$?
rcis "the probe runs through the runner" 0 "$rc" "$out"
want "the probe could write its own TMPDIR, so its other answers mean something" "Ok(())" "$(field WRITE_TMP "$out")"
want "the caller's config cannot be read" "NotFound" "$(field READ_CONFIG "$out")"
want "the caller's config directory is not there at all" "NotFound" "$(field CONFIG_DIR "$out")"
want "a write under the install's run directory lands in the sandbox's tmpfs" "Ok(())" "$(field WRITE_RUN "$out")"
[ ! -e "$home/spira/run/x" ] && ok "nothing the probe wrote reached the caller's home" || bad "nothing the probe wrote reached the caller's home"
want "the caller's ~/.local/bin is not there" "NotFound" "$(field LOCAL_BIN "$out")"
is "no SPIRA_ key of the caller's gets in, SPIRA_HOME is the workspace's own" "SPIRA_HOME" "$(field SPIRA_KEYS "$out")"
is "SPIRA_HOME names the workspace's spira directory" "$WORKSPACE/spira" "$(field SPIRA_HOME "$out")"
is "there is no network but loopback" "" "$(field NET_IFS "$out")"
want "the sandbox carries testenv's system git identity" 'email = spirauser@localhost' "$(field GIT_IDENTITY "$out")"

# Without bwrap it refuses, naming why, and runs nothing — in a root where the binary could
# still have left its marker, so its absence means it never ran.
printf '#!/bin/sh\ntouch %s/ran\n' "$TMP" > "$TMP/would-run"
chmod +x "$TMP/would-run"
hide=()
for d in /usr/local/bin /usr/bin /bin; do [ -e "$d/bwrap" ] && hide+=(--ro-bind /dev/null "$d/bwrap"); done
out="$(run bwrap --die-with-parent --ro-bind / / --dev /dev "${hide[@]}" --bind "$TMP" "$TMP" -- "$RUNNER" "$TMP/would-run" 2>&1)"
rc=$?
rcis "without bwrap the runner refuses" 2 "$rc" "$out"
want "the refusal names why" "bwrap (bubblewrap) is not on" "$out"
[ ! -e "$TMP/ran" ] && ok "without bwrap the binary never ran" || bad "without bwrap the binary never ran" "$out"

# Inside a sandbox that already holds the policy — testenv's sandbox unit runner runs cargo
# under bwrap — the runner makes no second sandbox; it clears the environment and runs there.
# --disable-userns: the runner cannot make a sandbox of its own in there, so only the held
# path can run the probe. The passwd home is hidden too, as the runner requires.
pwhome="$(getent passwd "$(id -u)" | cut -d: -f6)"
held=(--die-with-parent --unshare-user --disable-userns --unshare-net --ro-bind / / --dev /dev --proc /proc
      --tmpfs /tmp --tmpfs /run --tmpfs "$pwhome" --tmpfs "$home" --bind "$TMP/target" "$TMP/target")
out="$(run bwrap "${held[@]}" -- "$RUNNER" "${probe[@]}" 2>&1)"
rc=$?
rcis "inside a sandbox that holds the policy the probe runs" 0 "$rc" "$out"
is "inside it no SPIRA_ key of the caller's gets in either" "SPIRA_HOME" "$(field SPIRA_KEYS "$out")"
want "inside it the caller's config cannot be read" "NotFound" "$(field READ_CONFIG "$out")"
want "inside it the sandbox's git identity is still there" 'email = spirauser@localhost' "$(field GIT_IDENTITY "$out")"

# A sandbox that does NOT hold it (a writable root, the network) is not mistaken for one: the
# runner asks bwrap for its own (which this kernel may refuse to nest, or which hides $TMP),
# and the binary never runs where it could have left its marker.
rm -f "$TMP/ran"
out="$(run bwrap --die-with-parent --bind / / --dev /dev --proc /proc -- "$RUNNER" "$TMP/would-run" 2>&1)"
rc=$?
[ "$rc" != 0 ] && ok "a sandbox short of the policy fails the run" || bad "a sandbox short of the policy fails the run" "$out"
[[ "$out" == *"bwrap:"* || "$out" == *"touch:"* ]] && ok "the failure is bwrap's refusal or the sandboxed binary's" ||
    bad "the failure is bwrap's refusal or the sandboxed binary's" "$out"
[ ! -e "$TMP/ran" ] && ok "a sandbox short of the policy is not taken for one" || bad "a sandbox short of the policy is not taken for one" "ran unsandboxed: $out"

# `cargo run` keeps the caller's environment: cargo places its binaries beside .fingerprint.
mkdir -p "$TMP/target/debug/.fingerprint"
printf '#!/bin/sh\ncat "$HOME/.config/spira/planted"\n' > "$TMP/target/debug/a-workspace-bin"
chmod +x "$TMP/target/debug/a-workspace-bin"
is "a cargo run artifact runs as is" "planted = true" "$(run "$RUNNER" "$TMP/target/debug/a-workspace-bin" 2>&1)"

tl_summary
