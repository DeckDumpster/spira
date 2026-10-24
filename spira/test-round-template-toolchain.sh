#!/usr/bin/env bash
#
# test-round-template-toolchain.sh — template.sh installs the pinned toolchain over a different rustc.
#
# tier: T1
# covers: round-vm/src/template.sh
# host-reason: shell and fixture binaries only
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
ROOT="$(cd "$HERE/.." && pwd -P)"
. "$HERE/testlib.sh"

echo "test-round-template-toolchain.sh"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM
sed -n '/^# toolchain-begin$/,/^# toolchain-end$/p' "$ROOT/round-vm/src/template.sh" > "$TMP/snippet.sh"
[ -s "$TMP/snippet.sh" ] && ok "toolchain block found" || bad "toolchain block found" "markers missing"

mkdir -p "$TMP/bin"
cat > "$TMP/bin/rustup" <<'R'
#!/bin/sh
[ "$1 $2" = "toolchain install" ] || exit 1
d="$RUSTUP_HOME/toolchains/$3-x86_64-unknown-linux-gnu/bin"
mkdir -p "$d"
printf '#!/bin/sh\necho "rustc %s (fake)"\n' "$3" > "$d/rustc"; chmod +x "$d/rustc"
echo installed > "$RUSTUP_HOME/log"
R
chmod +x "$TMP/bin/rustup"

fixture() {
    ch="$TMP/$1"; mkdir -p "$ch/bin"
    printf '#!/bin/sh\necho "rustc %s (fake)"\n' "$2" > "$ch/bin/rustc"; chmod +x "$ch/bin/rustc"
}
run() { env -i PATH="$TMP/bin:/usr/bin:/bin" CARGO_HOME="$TMP/$1" toolchain="$2" bash "$TMP/snippet.sh" 2>&1; }

fixture old 1.82.0
out="$(run old 1.99.1)"
is "different rustc is replaced by the pin" "rustc 1.99.1 (fake)" "$("$TMP/old/bin/rustc" --version)"
want "log names pin and image version" "round toolchain 1.99.1 (installed at round time; the image has 1.82.0)" "$out"

fixture same 1.99.1
out="$(run same 1.99.1)"
[ ! -e "$TMP/same/rustup/log" ] && ok "matching rustc left alone" || bad "matching rustc left alone" "rustup ran"
is "matching rustc silent" "" "$out"

mkdir -p "$TMP/none/bin"
run none 1.99.1 >/dev/null
is "empty home gets the pin" "rustc 1.99.1 (fake)" "$("$TMP/none/bin/rustc" --version)"

sed -n '/^# default-begin$/,/^# default-end$/p' "$ROOT/round-vm/src/template.sh" > "$TMP/default.sh"
sed -n '/^# verify-begin$/,/^# verify-end$/p' "$ROOT/round-vm/src/template.sh" > "$TMP/verify.sh"
[ -s "$TMP/default.sh" ] && [ -s "$TMP/verify.sh" ] && ok "default and verify blocks found" || bad "default and verify blocks found" "markers missing"

mkdir -p "$TMP/dbin" "$TMP/home"
cat > "$TMP/dbin/rustup" <<'R'
#!/bin/sh
echo "$RUSTUP_HOME $*" >> "$HOME/rustup.log"
R
chmod +x "$TMP/dbin/rustup"
env -i PATH="$TMP/dbin:/usr/bin:/bin" HOME="$TMP/home" toolchain=1.99.1 bash "$TMP/default.sh" >/dev/null 2>&1
want "pin is installed in the ambient rustup home" "$TMP/home/.rustup toolchain install 1.99.1" "$(cat "$TMP/home/rustup.log")"
want "pin becomes the ambient default" "$TMP/home/.rustup default 1.99.1" "$(cat "$TMP/home/rustup.log")"

verify() { env -i PLAIN_PATH="$TMP/$1" PATH=/usr/bin:/bin HOME="$TMP/home" bash "$TMP/verify.sh" 2>&1; }
mkdir -p "$TMP/vbad" "$TMP/vgood"
printf '#!/bin/sh\necho "error: rustup could not choose a version of cargo to run" >&2\nexit 1\n' > "$TMP/vbad/cargo"
printf '#!/bin/sh\necho "cargo 1.99.1"\n' > "$TMP/vgood/cargo"
chmod +x "$TMP/vbad/cargo" "$TMP/vgood/cargo"
out="$(verify vbad)"; rc=$?
[ "$rc" -ne 0 ] && ok "clone where cargo fails is refused" || bad "clone where cargo fails is refused" "exit 0"
want "refusal carries cargo's error" "rustup could not choose a version" "$out"
verify vgood >/dev/null && ok "clone where cargo runs passes" || bad "clone where cargo runs passes" "refused"

tl_summary
