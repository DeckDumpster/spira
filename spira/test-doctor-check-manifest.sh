#!/usr/bin/env bash
#
# test-doctor-check-manifest.sh — doctor-check prints conf.sh's own error when its manifest
# will not load.
#
# covers: spira/testenv/doctor-check.sh

set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
T="$(mktemp -d "${TMPDIR:-/var/tmp}/doctor-check-manifest.XXXXXX")" || exit 1
trap 'rm -rf "$T"' EXIT
fail=0
check() { if [ "$2" = ok ]; then echo "ok   $1"; else echo "FAIL $1"; fail=1; fi; }

printf 'echo "conf.d.index is stale: planted marker" >&2\nreturn 1\n' > "$T/conf.sh"
out="$(env -i HOME="$T" PATH="$PATH" bash "$HERE/testenv/doctor-check.sh" "$T/conf.sh" 2>&1)"
rc=$?
[ "$rc" -ne 0 ] && check "a manifest that cannot load fails closed" ok || check "a manifest that cannot load fails closed" no
case "$out" in *"planted marker"*) check "conf.sh's own error is shown" ok ;; *) check "conf.sh's own error is shown" no ;; esac

printf 'spira_deps_list() { echo; }\n' > "$T/quiet.sh"
out="$(env -i HOME="$T" PATH="$PATH" bash "$HERE/testenv/doctor-check.sh" "$T/quiet.sh" 2>&1)"
case "$out" in *"planted marker"*) check "the marker is absent when conf.sh is silent" no ;; *) check "the marker is absent when conf.sh is silent" ok ;; esac
exit "$fail"
