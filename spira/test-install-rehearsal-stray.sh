#!/usr/bin/env bash
# test-install-rehearsal-stray.sh — uninstall's stray sweep against real systemd
#
# POSITIVE CONTROL (law-absence-needs-a-positive-control):
#   Plants a unit not in the owned manifest, runs uninstall, and requires "STRAY" and the
#   planted name in its output. Only then does a clean uninstall in test-install-rehearsal.sh
#   count as evidence that the sweep finds nothing.
#
# The sweep reads the unit directory against the manifest, so no install precedes it; the
# container, stubs and configure are the ones test-install-rehearsal.sh uses.
#
# SKIP CONDITION: no podman on PATH, or user systemd not available in the container.
#
# runtime: ~1m
# tier: T2
# bare-host: builds its own podman containers; the corpus container has no podman
# covers: spira/lib-test-install-rehearsal.sh spira/podman-exec-diag.sh install/src/bin/install.rs spira/uninstall.sh spira/configure.sh testenv/src/container.rs spira/testenv/Containerfile UC-instance-lifecycle-40
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"

. "$HERE/testlib.sh"
tl_config SPIRA_TEST_CASE_DEADLINE=180   # slow cases: real git/dolt on a loaded VM
. "$HERE/podman-exec-diag.sh"
iszero()  { [ "$2" = 0 ] && ok "$1" || bad "$1" "exit $2"; }

echo "test-install-rehearsal-stray.sh"

. "$HERE/lib-test-install-rehearsal.sh"

echo
echo "stray sweep positive control — plant a unit, confirm STRAY is reported:"
"${CEXEC[@]}" "$CNAME" bash -c \
    'mkdir -p "$HOME/.config/systemd/user" && printf "[Unit]\nDescription=stray\n" \
     > "$HOME/.config/systemd/user/spira-legacy-stray.service"' >/dev/null
iszero "stray unit planted" "$?"

stray_out="$("${INSTALL_EXEC[@]}" "$CNAME" \
    bash /workspace/spira/uninstall.sh --yes 2>&1)"
iszero "uninstall.sh --yes (with stray) exits 0" "$?"
want "stray sweep reports STRAY"          "STRAY"                        "$stray_out"
want "stray sweep names the planted unit" "spira-legacy-stray.service"   "$stray_out"

echo
tl_summary
