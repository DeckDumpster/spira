#!/usr/bin/env bash
# test-tmp-clean.sh — a suite leaves nothing in the temp directory it was given, however it
# made it and however it ended.
# tier: T1
# covers: spira/testlib.sh spira/lib-test-install-rehearsal.sh spira/test-*.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"

echo "test-tmp-clean.sh"
FIX="$(mktemp -d)"
trap 'chmod -R u+rwX "$FIX" 2>/dev/null; rm -rf "$FIX"' EXIT

# A suite doing what the leaking ones did: a bare mktemp -d with no trap, a read-only tree
# beside it, a trap that replaces any cleanup the library might have installed, a fixed-name file.
cat > "$FIX/leaky.sh" <<FIXTURE
. "$HERE/testlib.sh"
trap 'echo mine' EXIT
d="\$(mktemp -d)"; mkdir -p "\$d/bin"; : > "\$d/bin/tool"; chmod a-w "\$d/bin" "\$d"
: > "\${TMPDIR:-/tmp}/fixed-name.tar"
f="\$(mktemp)"; echo "\$f" > "$FIX/\$1.path"
FIXTURE
# The same litter from a script that never sources the library: the matcher's positive control.
cat > "$FIX/plain.sh" <<'FIXTURE'
d="$(mktemp -d)"; mkdir -p "$d/bin"; chmod a-w "$d/bin"
FIXTURE

leftovers() { find "$1" -mindepth 1 -maxdepth 1 2>/dev/null | wc -l; }
settle() {   # settle <base>: wait for the reaper, which acts once the suite's shell is gone
    local i
    for i in $(seq 1 100); do [ "$(leftovers "$1")" = 0 ] && return 0; sleep 0.1; done
    return 1
}

B1="$FIX/plain-base"; mkdir -p "$B1"
env TMPDIR="$B1" bash "$FIX/plain.sh"
sleep 1
is "a script without the library does leave litter (the matcher can see it)" 1 "$(leftovers "$B1")"
chmod -R u+rwX "$B1"

B2="$FIX/leaky-base"; mkdir -p "$B2"
env TMPDIR="$B2" bash "$FIX/leaky.sh" ended >/dev/null 2>&1
settle "$B2"
is "a finished suite leaves its temp directory empty" 0 "$(leftovers "$B2")"

B3="$FIX/killed-base"; mkdir -p "$B3"
env TMPDIR="$B3" bash "$FIX/leaky.sh" killed >/dev/null 2>&1 &
pid=$!
kill -KILL "$pid" 2>/dev/null; wait "$pid" 2>/dev/null
settle "$B3"
is "a killed suite leaves its temp directory empty" 0 "$(leftovers "$B3")"

tl_summary
