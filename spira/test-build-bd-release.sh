#!/usr/bin/env bash
#
# test-build-bd-release.sh — build-bd.sh --from-flake takes bd from the flake environment
#
# CASES
#   1. POSITIVE CONTROL: an environment with no bin/bd is refused.
#   2. An environment carrying bin/bd is verified and installed.
#   3. The script no longer downloads anything.
#
# tier: T1
# covers: spira/build-bd.sh UC-instance-lifecycle-05
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"

echo "test-build-bd-release.sh"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM

TAG="$(grep '^BD_TAG_PIN=' "$HERE/build-bd.sh" | head -1 | cut -d'"' -f2)"
[ -n "$TAG" ] || { printf 'SKIP test-build-bd-release.sh: could not read BD_TAG_PIN\n' >&2; exit 77; }

mkdir -p "$TMP/empty-env/bin" "$TMP/env/bin"
printf '#!/bin/sh\nexit 0\n' > "$TMP/env/bin/bd"
chmod +x "$TMP/env/bin/bd"

FAKE_HOME="$TMP/home"
mkdir -p "$FAKE_HOME/.local/bin"

echo ""
echo "1. POSITIVE CONTROL — an environment without bd is refused"
none_out="$(env -i PATH="$PATH" HOME="$FAKE_HOME" BD_FLAKE_ENV="$TMP/empty-env" bash "$HERE/build-bd.sh" --from-flake 2>&1)"
none_rc=$?
[ "$none_rc" -ne 0 ] && ok "missing bd causes non-zero exit" || bad "missing bd causes non-zero exit" "exited 0"
want "missing bd reported" "no bd in" "$none_out"

echo ""
echo "2. Environment with bd — verified and installed"
rel_out="$(env -i PATH="$PATH" HOME="$FAKE_HOME" BD_FLAKE_ENV="$TMP/env" bash "$HERE/build-bd.sh" --from-flake 2>&1)"
rel_rc=$?
[ "$rel_rc" -eq 0 ] && ok "--from-flake exits 0" || bad "--from-flake exits 0" "exit $rel_rc: $(printf '%s' "$rel_out" | tail -3)"
[ -x "$FAKE_HOME/.local/bin/bd" ] && ok "bd installed to fake home" || bad "bd installed to fake home" "not found"

echo ""
echo "3. No download remains"
if grep -Eq '\b(curl|wget)\b' "$HERE/build-bd.sh"; then bad "build-bd.sh fetches nothing" "curl/wget still present"; else ok "build-bd.sh fetches nothing"; fi

echo ""
echo "G9 — the build pin and the harness-side SPIRA_BD_TAG default are one value:"
CONF_TAG="$(sed -n 's/^ *: "\${SPIRA_BD_TAG:=\(.*\)}"$/\1/p' "$HERE/conf.d/SPIRA_BD_TAG" | head -1)"
[ -n "$CONF_TAG" ] && ok "positive control: the conf.d default is readable" \
    || bad "positive control: the conf.d default is readable" "no := line in conf.d/SPIRA_BD_TAG"
is "BD_TAG_PIN in build-bd.sh equals the SPIRA_BD_TAG default" "$TAG" "$CONF_TAG"

echo ""
tl_summary
