#!/usr/bin/env bash
#
# test-round-image-preflight.sh — the round-image preflight selects on the recipe inputs,
# runs the setup in a base-OS container, and names the failing line on a red.
#
# tier: T1
# covers: spira/round-image-preflight.sh spira/round-image-preflight-guest.sh round-vm/src/recipe.rs round-vm/ami-setup.sh gate.steps
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
ROOT="$(cd "$HERE/.." && pwd -P)"
. "$HERE/testlib.sh"
echo "test-round-image-preflight.sh"
P="$HERE/round-image-preflight.sh"
T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT

paths="$(bash -c ". <(sed -n '/^recipe_paths()/,/^}/p' '$P'); recipe_paths '$ROOT'")"
for f in round-vm/ami-setup.sh round-vm/src/template.sh rust-toolchain.toml flake.nix flake.lock spira/testenv/Containerfile; do
    want "recipe.rs input $f is listed" "$f" "$paths"
done
is "six inputs, no more" 6 "$(printf '%s\n' "$paths" | wc -l)"
while IFS= read -r f; do [ -f "$ROOT/$f" ] && ok "input exists: $f" || bad "input exists: $f" "missing"; done <<< "$paths"

mkdir -p "$T/repo/round-vm/src" "$T/repo/spira/testenv" "$T/bin"
cp "$ROOT/round-vm/src/recipe.rs" "$T/repo/round-vm/src/"
(cd "$T/repo" && echo '[toolchain]' > rust-toolchain.toml && echo 'channel = "1.82.0"' >> rust-toolchain.toml \
    && echo a > README && git init -q -b main && git add -A && git -c user.email=t@t -c user.name=t commit -qm base \
    && git checkout -qb br && echo b >> README && git -c user.email=t@t -c user.name=t commit -qam readme)
cat > "$T/bin/podman" <<'STUB'
#!/bin/sh
echo "$*" >> "$PODMAN_LOG"
if [ -n "${PODMAN_FAIL:-}" ]; then
    echo "+ cd /"; echo "+ cargo install --locked cargo-nextest"
    i=0; while [ $i -lt 100 ]; do echo "noise $i"; i=$((i+1)); done
    echo "error: rustc 1.82.0 is not supported"; exit 101
fi
exit 0
STUB
chmod +x "$T/bin/podman"
run() { env -i PATH="$T/bin:/usr/bin:/bin" HOME="$T" PODMAN_LOG="$T/log" PODMAN_FAIL="${PODMAN_FAIL:-}" SPIRA_PREFLIGHT_FOREIGN="" "$@"; }

: > "$T/log"
out="$(run bash "$P" --tree "$T/repo" --base main --head br 2>&1)"; rc=$?
is "an ordinary diff exits 0" 0 "$rc"
want "an ordinary diff is skipped" "skipped" "$out"
want "an ordinary diff reports a real fence count" "fence: round-image-preflight checked 6 recipe paths" "$out"
is "an ordinary diff runs no container" 0 "$(wc -c < "$T/log")"

(cd "$T/repo" && git checkout -q br && echo '# x' >> rust-toolchain.toml && git -c user.email=t@t -c user.name=t commit -qam toolchain)
out="$(run bash "$P" --tree "$T/repo" --base main --head br 2>&1)"; rc=$?
is "a recipe-touching diff exits 0 on a green setup" 0 "$rc"
want "the touched input is named" "rust-toolchain.toml" "$out"
want "a container ran" "run --rm" "$(cat "$T/log")"
want "the base-OS image is the pinned 26.04" "ubuntu:26.04" "$(cat "$T/log")"
want "the toolchain is read from the pin" "1.82.0" "$(cat "$T/log")"
want "wall time is recorded" "wall " "$out"
want "a ran preflight reports its fence line" "fence: round-image-preflight checked 6 recipe paths" "$out"

out="$(PODMAN_FAIL=1 run bash "$P" --tree "$T/repo" --base main --head br 2>&1)"; rc=$?
is "a failing setup is red" 1 "$rc"
want "the failing setup line is named" "failing setup line: + cargo install --locked cargo-nextest" "$out"
want "the output tail is shown" "rustc 1.82.0 is not supported" "$out"
is "the tail is at most 40 lines" 40 "$(printf '%s\n' "$out" | sed -n '/last 40 lines/,$p' | tail -n +2 | wc -l)"

G="$(cat "$ROOT/gate.steps")"
want "the gate runs the preflight" "step bash spira/round-image-preflight.sh" "$G"
