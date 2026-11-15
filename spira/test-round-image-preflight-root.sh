#!/usr/bin/env bash
#
# test-round-image-preflight.sh — the round image preflight refuses without a build-disk cache
# root, keeps every byte it writes under that root, and copies only the tree's source files.
#
# Podman is a stub that records its argv and the copy list the host mounts into the guest.
#
# tier: T1
# covers: spira/round-image-preflight.sh spira/round-image-preflight-guest.sh spira/conf.d/SPIRA_PREFLIGHT_CACHE_ROOT
# timeout: 120
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"

echo "test-round-image-preflight.sh"

W="$(mktemp -d)"; trap 'rm -rf "$W"' EXIT
mkdir -p "$W/bin" "$W/rec"
cat > "$W/bin/podman" <<'STUB'
#!/bin/sh
printf '%s\n' "$*" >> "$REC/argv"
printf 'TMPDIR=%s\n' "$TMPDIR" >> "$REC/env"
prev=""
for a in "$@"; do
    case "$prev:$a" in -v:*:/tree.list:ro) cp "${a%%:*}" "$REC/list" ;; esac
    prev="$a"
done
exit 0
STUB
chmod +x "$W/bin/podman"

T="$W/tree"
mkdir -p "$T/round-vm/src" "$T/target/debug" "$T/spira"
git -C "$T" init -q
echo 'const A: &str = f(include_str!("../ami-setup.sh"));' > "$T/round-vm/src/recipe.rs"
echo x > "$T/round-vm/ami-setup.sh"
printf '[toolchain]\nchannel = "1.90.0"\n' > "$T/rust-toolchain.toml"
echo 'fn main() {}' > "$T/spira/a.rs"
echo 'target/' > "$T/.gitignore"
head -c 52428800 /dev/zero > "$T/target/debug/huge.bin"
git -C "$T" add -A >/dev/null
echo 'untracked' > "$T/spira/new.txt"

# run_pf <key-value> [extra args]: the preflight in a minimal environment
run_pf() {
    local root="$1"; shift
    rm -f "$W/rec"/*
    env -i PATH="$W/bin:$PATH" SPIRA_RELEASE="${SPIRA_RELEASE:-}" HOME="$W/home" REC="$W/rec" \
        SPIRA_TOML="$(tl_layer SPIRA_PREFLIGHT_CACHE_ROOT="$root")" \
        bash "$HERE/round-image-preflight.sh" --tree "$T" --force "$@" >"$W/out" 2>"$W/err"
}

# a sysfs fixture whose device for $1 (a path) is a disk with rotational=$2
fixture_sysfs() {
    local path="$1" rot="$2" mm S="$W/sys-$2"
    mm="$(findmnt -n -o MAJ:MIN -T "$path" | head -n 1 | tr -d ' ')"
    mkdir -p "$S/devices/vdz/queue" "$S/devices/dm0/slaves" "$S/dev/block"
    echo "$rot" > "$S/devices/vdz/queue/rotational"
    ln -sfn ../../vdz "$S/devices/dm0/slaves/vdz"
    ln -sfn "$S/devices/dm0" "$S/dev/block/$mm"
    echo "$S"
}

SCRATCH="$W/root"
NONROT="$(fixture_sysfs "$W" 0)"
ROT="$(fixture_sysfs "$W" 1)"

run_pf "" --sysfs "$NONROT"; rc=$?
[ "$rc" = 75 ] || sed "s/^/# err: /" "$W/err"
wantrc "(a) an unset cache root refuses with 75" 75 "$rc"
want "(a) the refusal names the key" SPIRA_PREFLIGHT_CACHE_ROOT "$(cat "$W/err")"
[ ! -e "$W/rec/argv" ] && ok "(a) podman is not run" || bad "(a) podman is not run" "$(cat "$W/rec/argv")"

run_pf "relative/dir" --sysfs "$NONROT"; rc=$?
wantrc "(a) a relative cache root refuses with 75" 75 "$rc"

run_pf "$SCRATCH" --sysfs "$NONROT"; rc=$?
wantrc "(b) a cache root on a non-rotational disk runs" 0 "$rc"
argv="$(cat "$W/rec/argv" 2>/dev/null)"
want "(b) podman storage is under the root" "--root $SCRATCH/storage" "$argv"
srcs="$(grep -o -- '-v [^ ]*' <<<"$argv" | awk '{print $2}' | cut -d: -f1 | grep -v -e "^$T\$" -e 'round-image-preflight-guest.sh$')"
bad_srcs="$(printf '%s\n' "$srcs" | grep -v "^$SCRATCH/" || true)"
[ -n "$srcs" ] && [ -z "$bad_srcs" ] && ok "(b) every cache mount source is under the root" || bad "(b) every cache mount source is under the root" "srcs=[$srcs] outside=[$bad_srcs]"
want "(b) TMPDIR for podman is under the root" "TMPDIR=$SCRATCH/tmp" "$(cat "$W/rec/env")"

want "(b) podman runroot is under the root" "--runroot $SCRATCH/run" "$argv"
nowant "(b) podman is given no --tmpdir" "--tmpdir" "$argv"
[ "$(grep -c -- "--runroot $SCRATCH/run" "$W/rec/argv")" = "$(grep -c . "$W/rec/argv")" ] \
    && ok "(b) every podman call carries the runroot" || bad "(b) every podman call carries the runroot" "$argv"

# the package-list-only path: a diff that edits only the apt-get install line
git -C "$T" -c user.email=t@t -c user.name=t commit -qm base
printf 'apt-get install -y curl\n' > "$T/round-vm/ami-setup.sh"
git -C "$T" -c user.email=t@t -c user.name=t commit -qam base2
printf 'apt-get install -y curl git\n' > "$T/round-vm/ami-setup.sh"
git -C "$T" -c user.email=t@t -c user.name=t commit -qam pkgs
rm -f "$W/rec"/*
env -i PATH="$W/bin:$PATH" HOME="$W/home" REC="$W/rec" SPIRA_TOML="$(tl_layer SPIRA_PREFLIGHT_CACHE_ROOT="$SCRATCH")" \
    bash "$HERE/round-image-preflight.sh" --tree "$T" --base HEAD~1 --head HEAD --sysfs "$NONROT" >"$W/out" 2>"$W/err"; rc=$?
wantrc "(b) the package-list-only path runs" 0 "$rc"
want "(b) it took the package-list-only path" "package list installs" "$(cat "$W/out")"
argv="$(cat "$W/rec/argv" 2>/dev/null)"
want "(b) package-only podman carries the runroot" "--runroot $SCRATCH/run" "$argv"
nowant "(b) package-only podman is given no --tmpdir" "--tmpdir" "$argv"

run_pf "$SCRATCH" --sysfs "$ROT"; rc=$?
wantrc "(c) a root on a rotational disk refuses with 75" 75 "$rc"
want "(c) the refusal names the key" SPIRA_PREFLIGHT_CACHE_ROOT "$(cat "$W/err")"
want "(c) the refusal names the device" vdz "$(cat "$W/err")"
want "(c) the refusal names the resolved path" "$(realpath "$SCRATCH")" "$(cat "$W/err")"

run_pf "$SCRATCH" --sysfs "$W/no-such-sysfs"; rc=$?
wantrc "(c) an unreadable disk refuses with 75" 75 "$rc"

run_pf "$SCRATCH" --sysfs "$NONROT"
n="$(tr -cd '\0' < "$W/rec/list" | wc -c)"
[ "$n" -gt 0 ] && ok "(d) the copy list is non-empty" || bad "(d) the copy list is non-empty" "no list"
nowant "(d) the list holds no target/ path" "target/" "$(tr '\0' '\n' < "$W/rec/list")"
want "(d) the list holds the tracked source" "spira/a.rs" "$(tr '\0' '\n' < "$W/rec/list")"
want "(d) the list holds an untracked unignored file" "spira/new.txt" "$(tr '\0' '\n' < "$W/rec/list")"
total="$(cd "$T" && xargs -0 -a "$W/rec/list" du -cb 2>/dev/null | tail -n 1 | cut -f1)"
[ "${total:-99999999}" -lt 1048576 ] && ok "(d) the listed files total under 1 MB" || bad "(d) the listed files total under 1 MB" "total=$total"

copy_line="$(grep -F 'tar --null -C /src -T /tree.list' "$HERE/round-image-preflight-guest.sh" | sed 's/^.*&& //')"
copy_line="${copy_line//-C \/src/-C $T}"; copy_line="${copy_line//\/tree.list/$W/rec/list}"; copy_line="${copy_line//\/root\/tree/$W/copied}"
mkdir -p "$W/copied"
bash -c "set -o pipefail; $copy_line" 2>"$W/tarerr"; rc=$?
[ "$rc" = 0 ] || { echo "# line: $copy_line"; sed "s/^/# tar: /" "$W/tarerr"; }
wantrc "(d) the guest's own copy line accepts the recorded list" 0 "$rc"
[ -f "$W/copied/spira/a.rs" ] && [ -f "$W/copied/spira/new.txt" ] && [ ! -e "$W/copied/target" ] \
    && ok "(d) the guest's copy holds the source and no target/" || bad "(d) the guest's copy holds the source and no target/" "$(ls -R "$W/copied" | head)"

tl_summary
