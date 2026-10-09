#!/usr/bin/env bash
#
# test-build-sh.sh — build.sh runs `make build` and reports its failure, `--skip-build`
# never invokes make, and an unknown argument is a usage error.
#
# tier: T1
# covers: spira/build.sh UC-instance-lifecycle-01
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

echo "test-build-sh.sh"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM
mkdir -p "$TMP/stub"
MAKE_LOG="$TMP/make.log"
cat > "$TMP/stub/make" <<STUB
#!/bin/sh
echo "make \$*" >> "$MAKE_LOG"
exit "\${STUB_MAKE_RC:-0}"
STUB
chmod +x "$TMP/stub/make"

run_build() { PATH="$TMP/stub:$PATH" bash "$HERE/build.sh" "$@" 2>&1; }
make_calls() { [ -f "$MAKE_LOG" ] && wc -l < "$MAKE_LOG" | tr -d ' ' || echo 0; }

out="$(run_build)"; rc=$?
is "a plain build exits 0" 0 "$rc"
is "a plain build invokes make exactly once (positive control)" 1 "$(make_calls)"
want "the plain build invokes the build target" "build" "$(cat "$MAKE_LOG" 2>/dev/null)"
want "a plain build says it is done" "build.sh: done" "$out"

rm -f "$MAKE_LOG"
out="$(run_build --skip-build)"; rc=$?
is "--skip-build exits 0" 0 "$rc"
is "--skip-build never invokes make" 0 "$(make_calls)"
want "--skip-build names the binaries it expects prebuilt" "landing-pass" "$out"

out="$(STUB_MAKE_RC=3 run_build)"; rc=$?
is "a failing make fails the build" 1 "$rc"
want "the failure says make build failed" "make build failed" "$out"

out="$(run_build --bogus)"; rc=$?
is "an unknown argument exits 2" 2 "$rc"
want "an unknown argument is named" "unknown argument: --bogus" "$out"

tl_summary
