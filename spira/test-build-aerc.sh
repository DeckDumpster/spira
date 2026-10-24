#!/usr/bin/env bash
# tier: T1
# covers: spira/build-aerc.sh spira/build-tarball.sh flake.nix
#
# build-aerc.sh takes aerc from nix, never clones a third-party git host, and retries a
# failed fetch before naming its source.
set -uo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
. "$here/testlib.sh"
check() { if [ "$1" = ok ]; then ok "$2"; else bad "$2"; fi; }

mkdir -p "$work/stubs" "$work/envdir/bin"
cat > "$work/envdir/bin/aerc" <<'AERC'
#!/bin/sh
echo "aerc 0.22.0 +notmuch-5.6.0 (go1.25.0 amd64 linux 2025-10-18)"
AERC
chmod +x "$work/envdir/bin/aerc"

# git.sr.ht is unreachable: any git use fails and is logged.
cat > "$work/stubs/git" <<GIT
#!/bin/sh
echo "git \$*" >> "$work/git.log"
echo "fatal: unable to access 'https://git.sr.ht/~rjarry/aerc/': Failed to connect" >&2
exit 128
GIT
# nix fails its first FAIL_FIRST calls, then succeeds.
cat > "$work/stubs/nix" <<NIX
#!/bin/sh
n=\$(cat "$work/nix.count" 2>/dev/null || echo 0); n=\$((n + 1)); echo "\$n" > "$work/nix.count"
[ "\$n" -le "\${FAIL_FIRST:-0}" ] && { echo "error: unable to download" >&2; exit 1; }
echo "$work/envdir"
NIX
chmod +x "$work/stubs/git" "$work/stubs/nix"

run() {
    rm -f "$work/nix.count" "$work/git.log"; rm -rf "$work/out"
    env -i PATH="$work/stubs:/usr/bin:/bin" HOME="$work/home" BUILD_AERC_BACKOFF=0 BUILD_AERC_NIX_CANDIDATES= "$@" \
        bash "$here/build-aerc.sh" "$work/out" > "$work/stdout" 2> "$work/stderr"
}

run FAIL_FIRST=0; rc=$?
{ [ "$rc" = 0 ] && [ -x "$work/out/aerc" ]; } && check ok "builds aerc from nix" || check no "builds aerc from nix (rc=$rc)"
[ ! -e "$work/git.log" ] && check ok "git.sr.ht unreachable: no git use at all" || check no "build-aerc.sh invoked git"

run FAIL_FIRST=2; rc=$?
{ [ "$rc" = 0 ] && [ "$(cat "$work/nix.count")" = 3 ]; } && check ok "transient fetch failure is retried" || check no "retry (rc=$rc)"

run FAIL_FIRST=99 BUILD_AERC_ATTEMPTS=3; rc=$?
{ [ "$rc" = 1 ] && [ "$(cat "$work/nix.count")" = 3 ] && grep -q 'spira-env' "$work/stderr" && grep -q 'after 3 attempts' "$work/stderr"; } \
    && check ok "exhausted retries fail naming the source" || check no "exhausted retries (rc=$rc)"

mkdir -p "$work/profile/bin"
mv "$work/stubs/nix" "$work/profile/bin/nix"
run BUILD_AERC_NIX_CANDIDATES="$work/profile/bin/nix"; rc=$?
{ [ "$rc" = 0 ] && [ -x "$work/out/aerc" ]; } && check ok "nix off PATH is found at its profile location" || check no "profile-location nix (rc=$rc)"
run BUILD_AERC_NIX_CANDIDATES="$work/nowhere/nix"; rc=$?
{ [ "$rc" = 1 ] && grep -q "$work/nowhere/nix" "$work/stderr"; } && check ok "no nix: failure lists where it looked" || check no "no-nix message (rc=$rc)"

tl_summary
