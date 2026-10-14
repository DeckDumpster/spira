#!/usr/bin/env bash
#
# test-podman-exec-diag.sh — a podman exec that times out starting surfaces podman's own state.
#
# tier: T1
# covers: spira/podman-exec-diag.sh spira/test-install-rehearsal.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"
. "$HERE/podman-exec-diag.sh"

echo "test-podman-exec-diag.sh"
T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT
mkdir "$T/bin"
cat > "$T/bin/podman" <<'STUB'
#!/bin/sh
case "$1" in
    info) echo "STUB-PODMAN-INFO" ;;
    logs) echo "STUB-CONTAINER-LOG" ;;
    *) ;;
esac
STUB
chmod +x "$T/bin/podman"
cat > "$T/hang" <<'STUB'
#!/bin/sh
echo "Error: timed out waiting for file /x/userdata/y" >&2
echo x >> "$HANG_COUNT"
exit 126
STUB
cat > "$T/fail" <<'STUB'
#!/bin/sh
echo "plain failure" >&2
echo x >> "$HANG_COUNT"
exit 3
STUB
chmod +x "$T/hang" "$T/fail"
export PATH="$T/bin:$PATH" HANG_COUNT="$T/count"

out="$(podman_exec_diag c "$T/hang" 2>&1)"; rc=$?
want "hang prints podman info" "STUB-PODMAN-INFO" "$out"
want "hang prints the container log" "STUB-CONTAINER-LOG" "$out"
is "hang keeps the exit code" 126 "$rc"
is "hang retried exactly once" 2 "$(wc -l < "$T/count")"

: > "$T/count"
out="$(podman_exec_diag c "$T/fail" 2>&1)"; rc=$?
nowant "other failure prints no diagnostics" "STUB-PODMAN-INFO" "$out"
is "other failure exit code" 3 "$rc"
is "other failure not retried" 1 "$(wc -l < "$T/count")"
