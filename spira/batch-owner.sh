#!/usr/bin/env bash
# batch-owner.sh — the owner-file protocol shared by testenv-batch.sh's cleanup and
# its orphan sweep: a spira-batch-* container's owner file must not be removed while
# the container might still exist, or the sweep below can never find it again.
#
# Sourced by testenv-batch.sh; also sourced directly by its test so the mechanism can
# be exercised against real podman without paying for the full batch pipeline.

# _batch_owner_release <cname> <owner-file>
# Unlinks <owner-file> only once <cname> is confirmed absent from podman. Returns 1
# (and leaves the file in place) when the container still exists — the caller should
# log that case, since it means teardown did not do what it was asked.
_batch_owner_release() {
    local cname="$1" ownerfile="$2"
    podman container exists "$cname" 2>/dev/null && return 1
    rm -f "$ownerfile"
}

# _batch_claim_owner <owner-file> <my-pid>
# Claims <owner-file> for <my-pid>, UNLESS it already names a different pid that is
# still alive — a container name is keyed off BATCH_KEY (tree + selection + harness
# hash) so a REPEAT run of the same tree is meant to reuse it, but two runs that
# overlap in time compute the identical key too. Overwriting the file unconditionally
# (the old behavior) let the second one silently steal the claim; whichever of the
# two exited first then tore the shared container down in its own ordinary cleanup,
# killing the other's still-running suites by signal mid-flight (sp-cltz3) — a name
# collision, not a suite fault. Returns 1 without touching the file when a live,
# different owner already holds the claim; the caller must refuse to share.
_batch_claim_owner() {
    local ownerfile="$1" mypid="$2" existing
    if [ -f "$ownerfile" ]; then
        existing="$(cat "$ownerfile" 2>/dev/null)" || existing=""
        if [ -n "$existing" ] && [ "$existing" != "$mypid" ] && [ -d "/proc/$existing" ]; then
            return 1
        fi
    fi
    printf '%s\n' "$mypid" > "$ownerfile"
}

# _batch_sweep_dead_owners [name-prefix] — arm 1: an owner file names a PID that has
# exited. Reaps the container it names and the file itself. One line per sweep on
# stdout. name-prefix (default spira-batch-, the production scope) narrows the glob
# to the caller's own containers — see _batch_sweep_ownerless below for why an
# unscoped call against real podman is dangerous.
_batch_sweep_dead_owners() {
    local name_prefix="${1:-spira-batch-}" _sw_f _sw_pid _sw_cname
    for _sw_f in "/tmp/${name_prefix}"*.owner; do
        [ -f "$_sw_f" ] || continue
        _sw_pid="$(cat "$_sw_f" 2>/dev/null)" || continue
        [ -n "$_sw_pid" ] || continue
        [ -d "/proc/$_sw_pid" ] && continue  # still alive
        _sw_cname="${_sw_f#/tmp/}"; _sw_cname="${_sw_cname%.owner}"
        podman stop "$_sw_cname" >/dev/null 2>&1 || true
        podman rm   "$_sw_cname" >/dev/null 2>&1 || true
        podman volume rm "${_sw_cname}-cargo-reg" >/dev/null 2>&1 || true
        podman volume rm "${_sw_cname}-cargo-git" >/dev/null 2>&1 || true
        rm -rf "/tmp/${_sw_cname}" 2>/dev/null || true
        rm -f "$_sw_f"
        printf 'swept orphan container %s (owner pid %s gone)\n' "$_sw_cname" "$_sw_pid"
    done
}

# _batch_sweep_ownerless <min-age-seconds> [name-prefix] — arm 2: a container matching
# <name-prefix> (default spira-batch-, the production scope) with NO owner file at all,
# older than the given bound. name-prefix lets a test narrow the sweep to its own
# containers: at min-age=0 the age bound protects nothing, so an unscoped call against
# real podman stops every ownerless container on the host, not just the caller's own.
# One line per sweep on stdout.
_batch_sweep_ownerless() {
    local min_age="${1:-3600}" name_prefix="${2:-spira-batch-}" _sw_cname _sw_started _sw_started_epoch _sw_age
    for _sw_cname in $(podman ps -a --filter "name=^${name_prefix}" --format '{{.Names}}' 2>/dev/null); do
        [ -f "/tmp/${_sw_cname}.owner" ] && continue  # governed by the dead-owner arm
        _sw_started="$(podman container inspect --format '{{.State.StartedAt}}' "$_sw_cname" 2>/dev/null)" || continue
        _sw_started_epoch="$(date -d "$_sw_started" +%s 2>/dev/null)" || continue
        _sw_age=$(( $(date +%s) - _sw_started_epoch ))
        [ "$_sw_age" -ge "$min_age" ] || continue
        podman stop "$_sw_cname" >/dev/null 2>&1 || true
        podman rm   "$_sw_cname" >/dev/null 2>&1 || true
        podman volume rm "${_sw_cname}-cargo-reg" >/dev/null 2>&1 || true
        podman volume rm "${_sw_cname}-cargo-git" >/dev/null 2>&1 || true
        rm -rf "/tmp/${_sw_cname}" 2>/dev/null || true
        printf 'swept ownerless container %s (age %ss, no owner file)\n' "$_sw_cname" "$_sw_age"
    done
}
