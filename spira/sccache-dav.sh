#!/usr/bin/env bash
#
# sccache-dav.sh — the WebDAV compilation-cache store (sp-xjnzl), wrapped the way loom.sh
# wraps loom: systemd execs this so the binary's address and storage root come from
# environment variables the operator sets on the unit, not from a path baked in at build
# time. This store's config is deliberately not read from the operator's own harness config
# at all (SCCACHE_DAV_ADDR/SCCACHE_DAV_ROOT are this unit's own Environment= lines) — it is
# one box's own LAN endpoint, the same kind of fact round_vm_host_addr already is, not a
# value other tools need to discover.
set -uo pipefail

command -v sccache-dav >/dev/null 2>&1 || { printf 'sccache-dav: not found on PATH (%s)\n' "$PATH" >&2; exit 2; }

: "${SCCACHE_DAV_ADDR:?sccache-dav.sh: SCCACHE_DAV_ADDR must be set on the unit (e.g. 192.168.1.56:9431)}"
: "${SCCACHE_DAV_ROOT:?sccache-dav.sh: SCCACHE_DAV_ROOT must be set on the unit}"

exec sccache-dav
