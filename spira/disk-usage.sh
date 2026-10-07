#!/usr/bin/env bash
# disk-usage.sh — observed free-space percentage per path, for the reconciler's Disk
# invariant (sp-lkfto.3).
#
#   disk-usage.sh [<path>...]
#
# One line per path: "<path>\t<free-pct>" — an integer 0-100 read from `df`, or the
# sentinel "ERR" when an EXPLICITLY named path could not be read, so a typo in a Composite's
# DiskSpec.paths still gets a row (unobservable, never silently dropped) rather than
# vanishing from the check set (law-a-control-that-cannot-check-must-refuse).
#
# With NO arguments — the fallback for an install with no Composite DiskSpec — checks the
# harness's own default set: the root filesystem, the dolt data directory (SPIRA_DOLT_DATA),
# and podman's own storage root, asked of podman itself via `podman info` rather than
# guessed: rootless vs. rootful and an operator-set graphroot all put it somewhere
# different. A default-set path that cannot be read (podman not installed, dolt in
# server mode elsewhere) is simply omitted — there is no declared expectation it exist.
#
# covers: spira/conf.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/conf.sh"

explicit=1
paths=("$@")
if [ "${#paths[@]}" -eq 0 ]; then
    explicit=0
    paths=(/)
    [ -n "${SPIRA_DOLT_DATA:-}" ] && paths+=("$SPIRA_DOLT_DATA")
    pm_root="$(timeout 5 podman info --format '{{.Store.GraphRoot}}' 2>/dev/null)"
    [ -n "$pm_root" ] && paths+=("$pm_root")
fi

for p in "${paths[@]}"; do
    free_pct=""
    line="$(df -kP "$p" 2>/dev/null | tail -n1)"
    if [ -n "$line" ]; then
        read -r _ total _ avail _ _ <<< "$line"
        if [ -n "${total:-}" ] && [ "$total" -gt 0 ] 2>/dev/null; then
            free_pct=$(( avail * 100 / total ))
        fi
    fi
    if [ -n "$free_pct" ]; then
        printf '%s\t%s\n' "$p" "$free_pct"
    elif [ "$explicit" = 1 ]; then
        printf '%s\tERR\n' "$p"
    fi
done
