#!/usr/bin/env bash
# pve.sh — Proxmox VE operations for the concierge.
#
#   pve.sh clone <vmid> <newid> <name> [--pool <pool>] [--full]
#   pve.sh config <vmid>
#   pve.sh config <vmid> --set <key=value> [--set <key=value> ...]
#   pve.sh start <vmid>
#   pve.sh shutdown <vmid> [--timeout <s>]
#   pve.sh status <vmid>
#   pve.sh template <vmid>
#   pve.sh guest-exec <vmid> [--timeout <s>] <cmd> [args...]
#   pve.sh guest-exec-status <vmid> <pid>
#   pve.sh nextid
#   pve.sh list [--pool <pool>]
#
# Credentials are read from SPIRA_PVE_ENV (default: ~/.config/spira/pve.env).
# Required variables in that file:
#   PVE_TOKEN_ID     — API token id (user@realm!tokenname)
#   PVE_TOKEN_SECRET — API token secret
#   PVE_NODE         — Proxmox node name
#
# Optional variables (have defaults):
#   PVE_API_HOST  — hostname or IP (default: localhost)
#   PVE_API_PORT  — API port (default: 8006)
#   PVE_CACERT    — CA certificate path (default: /etc/pve/pve-root-ca.pem)
#
# Certificate is verified against PVE_CACERT. There is no --insecure fallback;
# a missing or unset cacert is a hard error so callers cannot silently skip
# verification by omitting it.
#
# Timeouts:
#   PVE_TASK_TIMEOUT — seconds to poll an async task (default: 300)
#   PVE_EXEC_TIMEOUT — seconds for guest-exec to complete (default: 30)
#   A long exec timeout blocks the caller's shell for its full duration; keep
#   PVE_EXEC_TIMEOUT short and pass --timeout when a command legitimately takes longer.

# covers: spira/pve.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/conf.sh"

_PVE_ENV="${SPIRA_PVE_ENV:-${XDG_CONFIG_HOME:-$HOME/.config}/spira/pve.env}"

if [ -f "$_PVE_ENV" ]; then
    # shellcheck source=/dev/null
    . "$_PVE_ENV"
fi

: "${PVE_TOKEN_ID:?pve.sh: PVE_TOKEN_ID not set — is ${_PVE_ENV} present and readable?}"
: "${PVE_TOKEN_SECRET:?pve.sh: PVE_TOKEN_SECRET not set — is ${_PVE_ENV} present and readable?}"
: "${PVE_NODE:?pve.sh: PVE_NODE not set — is ${_PVE_ENV} present and readable?}"
: "${PVE_API_HOST:=localhost}"
: "${PVE_API_PORT:=8006}"
: "${PVE_CACERT:=/etc/pve/pve-root-ca.pem}"

if [ ! -f "$PVE_CACERT" ]; then
    printf 'pve.sh: cacert not found: %s\n' "$PVE_CACERT" >&2
    printf 'pve.sh: set PVE_CACERT in %s to the path of the Proxmox CA certificate\n' "$_PVE_ENV" >&2
    printf 'pve.sh: fetch it from the hypervisor: scp <host>:/etc/pve/pve-root-ca.pem <local-path>\n' >&2
    exit 1
fi

_PVE_TASK_TIMEOUT="${PVE_TASK_TIMEOUT:-300}"
_PVE_EXEC_TIMEOUT="${PVE_EXEC_TIMEOUT:-30}"

_PVE_STATUS="" _PVE_BODY=""

_pve_api() {
    local method="$1" path="$2"
    shift 2
    _PVE_STATUS="" _PVE_BODY=""
    local _tmpfile _status _rc=0
    _tmpfile=$(mktemp)
    _status=$(curl -sS \
        --cacert "$PVE_CACERT" \
        -o "$_tmpfile" -w '%{http_code}' \
        -X "$method" \
        -H "Authorization: PVEAPIToken=${PVE_TOKEN_ID}=${PVE_TOKEN_SECRET}" \
        "https://${PVE_API_HOST}:${PVE_API_PORT}/api2/json${path}" \
        "$@") || _rc=$?
    _PVE_STATUS="$_status"
    _PVE_BODY=$(cat "$_tmpfile")
    rm -f "$_tmpfile"
    if [ "$_rc" -ne 0 ]; then
        printf 'pve.sh: curl transport error (method=%s path=%s)\n' "$method" "$path" >&2
        return "$_rc"
    fi
    case "$_PVE_STATUS" in
        2??) ;;
        *) printf 'pve.sh: HTTP %s (%s %s)\n%s\n' "$_PVE_STATUS" "$method" "$path" "$_PVE_BODY" >&2
           return 1 ;;
    esac
}

_pve_json_get() {
    python3 -c "
import json, sys
obj = json.load(sys.stdin)
keys = sys.argv[1:]
for k in keys:
    obj = obj.get(k, '') if isinstance(obj, dict) else ''
if isinstance(obj, (dict, list)):
    print(json.dumps(obj))
elif obj or obj == 0:
    print(obj)
" "$@" <<< "$_PVE_BODY"
}

_pve_data() { _pve_json_get "data"; }

_pve_poll_task() {
    local upid="$1" timeout="${2:-$_PVE_TASK_TIMEOUT}"
    local encoded deadline status exitstatus
    encoded="$(python3 -c 'import sys,urllib.parse; print(urllib.parse.quote(sys.argv[1],safe=""))' "$upid")"
    deadline=$(( $(date +%s) + timeout ))
    while true; do
        _pve_api GET "/nodes/${PVE_NODE}/tasks/${encoded}/status" || return 1
        status="$(_pve_json_get data status)"
        if [ "$status" = "stopped" ]; then
            exitstatus="$(_pve_json_get data exitstatus)"
            [ "$exitstatus" = "OK" ] && return 0
            printf 'pve.sh: task %s failed (exitstatus=%s)\n' "$upid" "$exitstatus" >&2
            return 1
        fi
        if [ "$(date +%s)" -ge "$deadline" ]; then
            printf 'pve.sh: timed out waiting for task (timeout=%ss)\n' "$timeout" >&2
            return 1
        fi
        sleep 2
    done
}

_pve_usage() {
    cat >&2 <<'EOF'
usage: pve.sh <verb> [args]

  clone <vmid> <newid> <name> [--pool <pool>] [--full]
  config <vmid> [--set <key=value> ...]
  start <vmid>
  shutdown <vmid> [--timeout <s>]
  status <vmid>
  template <vmid>
  guest-exec <vmid> [--timeout <s>] <cmd> [args...]
  guest-exec-status <vmid> <pid>
  nextid
  list [--pool <pool>]
EOF
    exit 1
}

[ $# -ge 1 ] || _pve_usage
verb="$1"; shift

case "$verb" in

clone)
    [ $# -ge 3 ] || { printf 'pve.sh clone: usage: pve.sh clone <vmid> <newid> <name> [--pool <pool>] [--full]\n' >&2; exit 1; }
    src="$1" dst="$2" name="$3"; shift 3
    pool="" full=0
    while [ $# -gt 0 ]; do
        case "$1" in
            --pool) pool="$2"; shift 2 ;;
            --full) full=1; shift ;;
            *) printf 'pve.sh clone: unknown option: %s\n' "$1" >&2; exit 1 ;;
        esac
    done
    args=(--data-urlencode "newid=${dst}" \
          --data-urlencode "name=${name}" \
          --data-urlencode "full=${full}")
    [ -n "$pool" ] && args+=(--data-urlencode "pool=${pool}")
    _pve_api POST "/nodes/${PVE_NODE}/qemu/${src}/clone" "${args[@]}" || exit 1
    upid="$(_pve_data)"
    printf 'pve.sh: clone task %s\n' "$upid" >&2
    _pve_poll_task "$upid"
    ;;

config)
    [ $# -ge 1 ] || { printf 'pve.sh config: usage: pve.sh config <vmid> [--set key=value ...]\n' >&2; exit 1; }
    vmid="$1"; shift
    if [ $# -eq 0 ]; then
        _pve_api GET "/nodes/${PVE_NODE}/qemu/${vmid}/config" || exit 1
        _pve_data
    else
        args=()
        while [ $# -gt 0 ]; do
            case "$1" in
                --set) args+=(--data-urlencode "$2"); shift 2 ;;
                *) printf 'pve.sh config: unknown option: %s\n' "$1" >&2; exit 1 ;;
            esac
        done
        _pve_api POST "/nodes/${PVE_NODE}/qemu/${vmid}/config" "${args[@]}" || exit 1
    fi
    ;;

start)
    [ $# -ge 1 ] || { printf 'pve.sh start: usage: pve.sh start <vmid>\n' >&2; exit 1; }
    _pve_api POST "/nodes/${PVE_NODE}/qemu/${1}/status/start" || exit 1
    upid="$(_pve_data)"
    [ -n "$upid" ] && _pve_poll_task "$upid" || true
    ;;

shutdown)
    [ $# -ge 1 ] || { printf 'pve.sh shutdown: usage: pve.sh shutdown <vmid> [--timeout <s>]\n' >&2; exit 1; }
    vmid="$1"; shift
    timeout="$_PVE_TASK_TIMEOUT"
    while [ $# -gt 0 ]; do
        case "$1" in
            --timeout) timeout="$2"; shift 2 ;;
            *) printf 'pve.sh shutdown: unknown option: %s\n' "$1" >&2; exit 1 ;;
        esac
    done
    _pve_api POST "/nodes/${PVE_NODE}/qemu/${vmid}/status/shutdown" || exit 1
    upid="$(_pve_data)"
    [ -n "$upid" ] && _pve_poll_task "$upid" "$timeout" || true
    ;;

status)
    [ $# -ge 1 ] || { printf 'pve.sh status: usage: pve.sh status <vmid>\n' >&2; exit 1; }
    _pve_api GET "/nodes/${PVE_NODE}/qemu/${1}/status/current" || exit 1
    _pve_data
    ;;

template)
    [ $# -ge 1 ] || { printf 'pve.sh template: usage: pve.sh template <vmid>\n' >&2; exit 1; }
    _pve_api POST "/nodes/${PVE_NODE}/qemu/${1}/template" || exit 1
    upid="$(_pve_data)"
    [ -n "$upid" ] && _pve_poll_task "$upid" || true
    ;;

guest-exec)
    [ $# -ge 2 ] || { printf 'pve.sh guest-exec: usage: pve.sh guest-exec <vmid> [--timeout <s>] <cmd> [args...]\n' >&2; exit 1; }
    vmid="$1"; shift
    timeout="$_PVE_EXEC_TIMEOUT"
    [ "${1:-}" = "--timeout" ] && { timeout="$2"; shift 2; }
    [ $# -ge 1 ] || { printf 'pve.sh guest-exec: no command given\n' >&2; exit 1; }
    api_args=(--data-urlencode "capture-output=1")
    for a in "$@"; do
        api_args+=(--data-urlencode "command=${a}")
    done
    _pve_api POST "/nodes/${PVE_NODE}/qemu/${vmid}/agent/exec" "${api_args[@]}" || exit 1
    pid="$(_pve_json_get data pid)"
    if [ -z "$pid" ]; then
        printf 'pve.sh: guest-exec returned no pid\n' >&2
        exit 1
    fi
    deadline=$(( $(date +%s) + timeout ))
    while true; do
        _pve_api GET "/nodes/${PVE_NODE}/qemu/${vmid}/agent/exec-status?pid=${pid}" || exit 1
        exited="$(_pve_json_get data exited)"
        if [ "$exited" = "1" ]; then
            _pve_data
            exitcode="$(_pve_json_get data exitcode)"
            exit "${exitcode:-0}"
        fi
        if [ "$(date +%s)" -ge "$deadline" ]; then
            printf 'pve.sh: guest-exec timed out after %ss (pid=%s)\n' "$timeout" "$pid" >&2
            exit 1
        fi
        sleep 1
    done
    ;;

guest-exec-status)
    [ $# -ge 2 ] || { printf 'pve.sh guest-exec-status: usage: pve.sh guest-exec-status <vmid> <pid>\n' >&2; exit 1; }
    vmid="$1" pid="$2"
    _pve_api GET "/nodes/${PVE_NODE}/qemu/${vmid}/agent/exec-status?pid=${pid}" || exit 1
    _pve_data
    ;;

nextid)
    _pve_api GET "/cluster/nextid" || exit 1
    _pve_data
    ;;

list)
    pool=""
    while [ $# -gt 0 ]; do
        case "$1" in
            --pool) pool="$2"; shift 2 ;;
            *) printf 'pve.sh list: unknown option: %s\n' "$1" >&2; exit 1 ;;
        esac
    done
    if [ -n "$pool" ]; then
        _pve_api GET "/pools/${pool}" || exit 1
    else
        _pve_api GET "/nodes/${PVE_NODE}/qemu" || exit 1
    fi
    _pve_data
    ;;

*)
    _pve_usage
    ;;
esac
