#!/usr/bin/env bash
#
# test-round-image-preflight.sh — which check round-image-preflight.sh runs for a diff.
#
# tier: T2
# covers: spira/round-image-preflight.sh spira/round-image-preflight-guest.sh round-vm/ami-setup.sh round-vm/src/recipe.rs
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$HERE/testlib.sh"
echo "test-round-image-preflight.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
R="$TMP/repo"; mkdir -p "$R/round-vm/src" "$TMP/bin" "$TMP/cache"
cd "$R" || exit 2
git init -q -b main . && git config user.email t@t && git config user.name t
cat > round-vm/src/recipe.rs <<'RS'
const A: &str = concat!(include_str!("../ami-setup.sh")).len();
const B: &str = concat!(include_str!("../../flake.lock")).len();
RS
printf 'channel-line\n' > flake.lock
printf '[toolchain]\nchannel = "1.99.0"\n' > rust-toolchain.toml
printf '#!/bin/bash\napt-get update -qq\napt-get install -y -qq podman git\nsystemctl enable ssh\n' > round-vm/ami-setup.sh
git add -A && git commit -qm base

cat > "$TMP/bin/podman" <<'SH'
#!/usr/bin/env bash
{ echo "ARGS: $*"; if [[ " $* " == *" -i "* ]]; then echo "STDIN:"; cat; fi; echo "END"; } >> "$PODMAN_LOG"
SH
chmod +x "$TMP/bin/podman"

run() {   # run <branch> [flags] -> output in $OUT, podman calls in $LOG
    git checkout -q "$1"; export PODMAN_LOG="$TMP/podman.log"; : > "$PODMAN_LOG"
    OUT="$(env -i PATH="$TMP/bin:$PATH" HOME="$TMP" PODMAN_LOG="$PODMAN_LOG" SPIRA_PREFLIGHT_CACHE="$TMP/cache" \
        bash "$HERE/round-image-preflight.sh" --tree "$R" --base main --head "$1" "${@:2}" 2>&1)"
    LOG="$(cat "$PODMAN_LOG")"
}
branch() { git checkout -q -b "$1" main; }

branch apt-only
sed -i 's/podman git$/podman git bubblewrap/' round-vm/ami-setup.sh
git commit -qam apt
branch apt-and-more; git checkout -q apt-only -- . ; sed -i 's/enable ssh/enable --now ssh/' round-vm/ami-setup.sh; git commit -qam both
git checkout -q main; branch lock; echo x >> flake.lock; git commit -qam lock
git checkout -q main; branch apt-twice; printf 'apt-get install -y -qq vim\n' >> round-vm/ami-setup.sh; git commit -qam twice
git checkout -q main; branch apt-gone; sed -i '/^apt-get install/d' round-vm/ami-setup.sh; git commit -qam gone
git checkout -q main; branch other-line; sed -i 's/enable ssh/enable --now ssh/' round-vm/ami-setup.sh; git commit -qam other
git checkout -q main; branch none; echo y > unrelated; git add -A; git commit -qm none

run apt-only
is "package-list change runs podman once" 1 "$(grep -c '^ARGS' <<<"$LOG")"
want "payload runs apt-get update" "apt-get update -qq" "$LOG"
want "payload carries the head's apt line" "apt-get install -y -qq podman git bubblewrap" "$LOG"
is "payload is update then the head's apt line" $'apt-get update -qq\napt-get install -y -qq podman git bubblewrap' "$(sed -n '/^apt-get update/,/^END/{/^END/!p}' <<<"$LOG")"
nowant "package-list change does not run the full setup" "/guest.sh" "$LOG"
want "package-list change says so" "package list installed" "$OUT"
nowant "payload is not on argv" "bubblewrap" "$(grep '^ARGS' <<<"$LOG")"

run apt-and-more
want "apt line plus another line runs the full setup" "/guest.sh" "$LOG"
nowant "full setup is not the package-list check" "package list installed" "$OUT"

for b in apt-twice apt-gone other-line; do
    run "$b"
    want "$b runs the full setup" "/guest.sh" "$LOG"
done

run lock
want "flake.lock change runs the full setup" "/guest.sh" "$LOG"

run apt-only --force
want "--force runs the full setup" "/guest.sh" "$LOG"

printf '#!/usr/bin/env bash\ncat >/dev/null; echo "E: Unable to locate package nope"; exit 100\n' > "$TMP/bin/podman"
run apt-only
RC=$(env -i PATH="$TMP/bin:$PATH" HOME="$TMP" SPIRA_PREFLIGHT_CACHE="$TMP/cache" bash "$HERE/round-image-preflight.sh" --tree "$R" --base main --head apt-only >/dev/null 2>&1; echo $?)
is "a failing package install exits 1" 1 "$RC"
want "a failing install prints the RED block" "round-image-preflight: RED rc=100" "$OUT"
want "the RED block carries the install output" "Unable to locate package nope" "$OUT"
nowant "a failing install is not reported as installed" "package list installs" "$OUT"

run none
is "no recipe input skips podman" "" "$LOG"
want "skip keeps its fence line" "(0 touched, setup not run)" "$OUT"

tl_summary
