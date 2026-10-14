#!/usr/bin/env bash
# Sourced. podman_exec_diag <container> <cmd...> runs the command; when podman itself failed to
# start the exec ("timed out waiting for file"), it prints podman's own state and retries once,
# since the command never ran. Any other failure passes through untouched.
podman_exec_diag() {
    local name="$1" log rc try
    shift
    log="$(mktemp)"
    for try in 1 2; do
        "$@" 2>"$log"
        rc=$?
        cat "$log" >&2
        grep -q 'timed out waiting for file' "$log" || break
        {
            echo "podman-exec-diag: podman could not start an exec in $name (attempt $try, exit $rc)"
            podman info 2>&1 | head -60
            podman ps -a --filter "name=$name" 2>&1
            podman logs --tail 50 "$name" 2>&1
            df -h /var/tmp "${XDG_DATA_HOME:-$HOME/.local/share}/containers" 2>&1
            podman version 2>&1 | head -5
            conmon --version 2>&1 | head -2
            crun --version 2>&1 | head -1
        } >&2
        [ "$try" = 1 ] && echo "podman-exec-diag: retrying once; the exec never started" >&2
    done
    rm -f "$log"
    return "$rc"
}
