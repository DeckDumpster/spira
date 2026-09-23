#!/usr/bin/env bash
# pve-role.sh — Create (or verify) the ConciergeVM role and a scoped token.
#
#   pve-role.sh create   — create the role, a scoped token, and bind ACLs
#   pve-role.sh verify   — confirm the role, token, and ACL bindings are present
#   pve-role.sh revoke   — revoke the unscoped token (run AFTER verifying the scoped one works)
#
# Run this ONCE from a session that already holds an unscoped credential (to create
# the role), then verify the scoped token works, then run 'revoke' to retire the
# unscoped one. These are operator steps, not aeon steps.
#
# Credentials: reads SPIRA_PVE_ENV (default: ~/.config/spira/pve.env).
# Additional required variables for 'create':
#   PVE_TEMPLATE_VMID — VMID of the runner template to grant access to
#   PVE_RUNNER_POOL   — pool name for ephemeral CI runners
#   PVE_STORAGE       — storage pool where clones are allocated (e.g. local-zfs)
#   PVE_SCOPED_USER   — user to bind the scoped token to (default: root@pam)
#   PVE_SCOPED_TOKEN  — name for the scoped token (default: concierge)
#   PVE_UNSCOPED_TOKEN — name of the old unscoped token to revoke (for 'revoke')
#
# The ConciergeVM role carries:
#   VM.Clone, VM.Config.Disk, VM.Config.Memory, VM.Config.Network,
#   VM.Config.Options, VM.PowerMgmt, VM.Monitor, VM.Audit,
#   VM.GuestAgent, Datastore.AllocateSpace
# It does NOT carry VM.Allocate (which includes destroy) or Sys.*.

# covers: spira/pve-role.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/conf.sh"

_PVE_ENV="${SPIRA_PVE_ENV:-${XDG_CONFIG_HOME:-$HOME/.config}/spira/pve.env}"
if [ -f "$_PVE_ENV" ]; then
    # shellcheck source=/dev/null
    . "$_PVE_ENV"
fi

: "${PVE_TOKEN_ID:?pve-role.sh: PVE_TOKEN_ID not set}"
: "${PVE_TOKEN_SECRET:?pve-role.sh: PVE_TOKEN_SECRET not set}"
: "${PVE_API_HOST:=localhost}"
: "${PVE_API_PORT:=8006}"
: "${PVE_CACERT:=/etc/pve/pve-root-ca.pem}"
: "${PVE_SCOPED_USER:=root@pam}"
: "${PVE_SCOPED_TOKEN:=concierge}"

_ROLE=ConciergeVM
_PRIVS="VM.Clone,VM.Config.Disk,VM.Config.Memory,VM.Config.Network,VM.Config.Options,VM.PowerMgmt,VM.Monitor,VM.Audit,VM.GuestAgent,Datastore.AllocateSpace"

if [ ! -f "$PVE_CACERT" ]; then
    printf 'pve-role.sh: cacert not found: %s\n' "$PVE_CACERT" >&2
    exit 1
fi

_pve() {
    local method="$1" path="$2"; shift 2
    local _tmp _status _rc=0
    _tmp=$(mktemp)
    _status=$(curl -sS \
        --cacert "$PVE_CACERT" \
        -o "$_tmp" -w '%{http_code}' \
        -X "$method" \
        -H "Authorization: PVEAPIToken=${PVE_TOKEN_ID}=${PVE_TOKEN_SECRET}" \
        "https://${PVE_API_HOST}:${PVE_API_PORT}/api2/json${path}" \
        "$@") || _rc=$?
    _PVE_LAST="$(cat "$_tmp")"; rm -f "$_tmp"
    [ "$_rc" -ne 0 ] && { printf 'pve-role.sh: curl error (%s %s)\n' "$method" "$path" >&2; return 1; }
    case "$_status" in 2??) ;; *)
        printf 'pve-role.sh: HTTP %s (%s %s)\n%s\n' "$_status" "$method" "$path" "$_PVE_LAST" >&2
        return 1 ;;
    esac
}
_PVE_LAST=""

_do_create() {
    : "${PVE_TEMPLATE_VMID:?pve-role.sh: PVE_TEMPLATE_VMID required for create}"
    : "${PVE_RUNNER_POOL:?pve-role.sh: PVE_RUNNER_POOL required for create}"
    : "${PVE_STORAGE:?pve-role.sh: PVE_STORAGE required for create}"

    printf 'pve-role.sh: creating role %s\n' "$_ROLE" >&2
    _pve POST /access/roles \
        --data-urlencode "roleid=${_ROLE}" \
        --data-urlencode "privs=${_PRIVS}" || exit 1

    local token_id="${PVE_SCOPED_USER}!${PVE_SCOPED_TOKEN}"
    local user_path token_secret
    user_path="${PVE_SCOPED_USER//@/%40}"
    printf 'pve-role.sh: creating scoped token %s (privsep=1)\n' "$token_id" >&2
    _pve POST "/access/users/${user_path}/token/${PVE_SCOPED_TOKEN}" \
        --data-urlencode "privsep=1" \
        --data-urlencode "comment=Scoped concierge token; role=${_ROLE}" || exit 1
    token_secret="$(printf '%s' "$_PVE_LAST" | python3 -c \
        'import json,sys; print(json.load(sys.stdin).get("data",{}).get("value",""))')"
    printf 'pve-role.sh: scoped token secret: %s\n' "$token_secret"
    printf 'pve-role.sh: add to pve.env:\n' >&2
    printf '  PVE_TOKEN_ID=%s\n' "$token_id" >&2
    printf '  PVE_TOKEN_SECRET=%s\n' "$token_secret" >&2

    printf 'pve-role.sh: binding ACL on /pool/%s\n' "$PVE_RUNNER_POOL" >&2
    _pve PUT /access/acl \
        --data-urlencode "path=/pool/${PVE_RUNNER_POOL}" \
        --data-urlencode "tokens=${token_id}" \
        --data-urlencode "roles=${_ROLE}" \
        --data-urlencode "propagate=1" || exit 1

    printf 'pve-role.sh: binding ACL on /vms/%s (template)\n' "$PVE_TEMPLATE_VMID" >&2
    _pve PUT /access/acl \
        --data-urlencode "path=/vms/${PVE_TEMPLATE_VMID}" \
        --data-urlencode "tokens=${token_id}" \
        --data-urlencode "roles=${_ROLE}" \
        --data-urlencode "propagate=0" || exit 1

    printf 'pve-role.sh: binding Datastore.AllocateSpace ACL on /storage/%s\n' "$PVE_STORAGE" >&2
    _pve PUT /access/acl \
        --data-urlencode "path=/storage/${PVE_STORAGE}" \
        --data-urlencode "tokens=${token_id}" \
        --data-urlencode "roles=${_ROLE}" \
        --data-urlencode "propagate=0" || exit 1

    printf 'pve-role.sh: done. Verify with: pve-role.sh verify\n' >&2
    printf 'pve-role.sh: after verifying the scoped token works, run: pve-role.sh revoke\n' >&2
}

_do_verify() {
    local token_id="${PVE_SCOPED_USER}!${PVE_SCOPED_TOKEN}"

    printf 'pve-role.sh: checking role %s\n' "$_ROLE" >&2
    _pve GET "/access/roles/${_ROLE}" || { printf 'pve-role.sh: role not found\n' >&2; exit 1; }
    printf '%s\n' "$_PVE_LAST"

    printf 'pve-role.sh: checking ACL entries for %s\n' "$token_id" >&2
    _pve GET "/access/acl" || exit 1
    printf '%s' "$_PVE_LAST" | python3 -c "
import json, sys
data = json.load(sys.stdin).get('data', [])
matches = [e for e in data if e.get('ugid') == sys.argv[1]]
for e in matches:
    print('  path=%s role=%s propagate=%s' % (e.get('path',''), e.get('roleid',''), e.get('propagate','')))
if not matches:
    print('  (none found)')
" "$token_id"
}

_do_revoke() {
    : "${PVE_UNSCOPED_TOKEN:?pve-role.sh: PVE_UNSCOPED_TOKEN required for revoke}"
    local user_path token_id
    user_path="${PVE_SCOPED_USER//@/%40}"
    token_id="${PVE_SCOPED_USER}!${PVE_UNSCOPED_TOKEN}"
    printf 'pve-role.sh: revoking unscoped token %s\n' "$token_id" >&2
    _pve DELETE "/access/users/${user_path}/token/${PVE_UNSCOPED_TOKEN}" || exit 1
    printf 'pve-role.sh: %s revoked\n' "$token_id" >&2
}

[ $# -ge 1 ] || { printf 'usage: pve-role.sh create|verify|revoke\n' >&2; exit 1; }
case "$1" in
    create)  _do_create ;;
    verify)  _do_verify ;;
    revoke)  _do_revoke ;;
    *) printf 'pve-role.sh: unknown command: %s\n' "$1" >&2; exit 1 ;;
esac
