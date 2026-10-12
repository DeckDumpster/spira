#!/usr/bin/env bash
# test-nix-gc.sh — nix-gc.sh collects only when the current spira-env resolves, and the
# unit that runs it is fenced and weekly.
# tier: T1
# covers: spira/nix-gc.sh systemd/spira-nix-gc.service systemd/spira-nix-gc.timer
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"
. "$HERE/nix-gc.sh"

echo "test-nix-gc.sh"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
mkdir -p "$TMP/fakebin" "$TMP/store/cur/bin" "$TMP/store/prev/bin"
printf '#!/bin/sh\necho collected >> "%s/calls"\n' "$TMP" > "$TMP/fakebin/nix-collect-garbage"
chmod +x "$TMP/fakebin/nix-collect-garbage"
export PATH="$TMP/fakebin:$PATH"
export SPIRA_NIX_ENV_LINK="$TMP/spira-env"

calls() { [ -f "$TMP/calls" ] && wc -l < "$TMP/calls" || echo 0; }

nix_gc_main >/dev/null 2>&1; rc=$?
[ "$rc" != 0 ] && [ "$(calls)" = 0 ] && ok "no current env: refuses, collects nothing" || bad "no current env: refuses, collects nothing" "rc=$rc calls=$(calls)"

ln -s "$TMP/store/cur" "$TMP/spira-env"
nix_gc_main >/dev/null 2>&1; rc=$?
[ "$rc" = 0 ] && [ "$(calls)" = 1 ] && ok "current env present: collects" || bad "current env present: collects" "rc=$rc calls=$(calls)"

ln -s "$TMP/store/gone" "$TMP/spira-env.prev"
nix_gc_main >/dev/null 2>&1; rc=$?
[ "$rc" != 0 ] && [ "$(calls)" = 1 ] && ok "dangling prev: refuses" || bad "dangling prev: refuses" "rc=$rc calls=$(calls)"

rm "$TMP/spira-env.prev"; ln -s "$TMP/store/prev" "$TMP/spira-env.prev"
nix_gc_main >/dev/null 2>&1; rc=$?
[ "$rc" = 0 ] && [ "$(calls)" = 2 ] && ok "both roots resolve: collects" || bad "both roots resolve: collects" "rc=$rc calls=$(calls)"

svc="$HERE/../systemd/spira-nix-gc.service"; tmr="$HERE/../systemd/spira-nix-gc.timer"
grep -q '^CPUQuota=' "$svc" && ok "unit is CPU-fenced" || bad "unit is CPU-fenced" "no CPUQuota"
grep -q '^OnCalendar=weekly' "$tmr" && ok "timer is weekly" || bad "timer is weekly" "no weekly OnCalendar"

tl_summary
