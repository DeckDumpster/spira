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

tl_summary
