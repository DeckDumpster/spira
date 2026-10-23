#!/usr/bin/env bash
#
# test-podman-store-shared.sh — podman in an aeon session reaches the same store as the host's:
#   a volume created through the session's podman is a directory under the store's backing path,
#   and one created under that path's mount is listed by the session's podman.
#
# covers: testenv/src/container.rs testenv/src/run.rs
# requires: podman
# host-reason: needs the host's rootless podman store and its bind mount; not available in the container
# tier: T4
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

command -v podman >/dev/null 2>&1 || skip "no podman on this host"
BACKING="$(findmnt -n -o SOURCE --target "$HOME/.local/share/containers/storage" 2>/dev/null | sed 's/.*\[\(.*\)\]/\1/')"
# batch-job: podman hang guard
STORE="$(timeout 120 podman info --format '{{.Store.GraphRoot}}' 2>/dev/null)"
[ -n "$STORE" ] || skip "podman reports no graph root"
REAL="$(realpath "$STORE")"

V="probe-store-shared-$$"
# batch-job: podman hang guard
trap 'timeout 120 podman volume rm "$V" >/dev/null 2>&1' EXIT
# batch-job: podman hang guard
timeout 120 podman volume create "$V" >/dev/null 2>&1
# batch-job: podman hang guard
is "a volume created in this context is listed" "1" "$(timeout 120 podman volume ls -q | grep -cx "$V")"
is "it exists as a directory under the store's graph root" "yes" "$([ -d "$REAL/volumes/$V" ] && echo yes || echo no)"
if [ -n "$BACKING" ] && [ -d "$HOME/spira$BACKING" ]; then
    is "the same directory is visible at the bind mount's backing path" "yes" "$([ -d "$HOME/spira$BACKING/volumes/$V" ] && echo yes || echo no)"
fi
# batch-job: podman hang guard
is "a container can mount it" "0" "$(timeout 120 podman run --rm -v "$V:/v" "$(timeout 120 podman images -q | head -1)" true >/dev/null 2>&1; echo $?)"
tl_summary
