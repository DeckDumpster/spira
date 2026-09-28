# round-vm-provider-pve.sh — the Proxmox implementation of round-vm.sh's provider seam.
#
# Sourced by round-vm.sh, never executed directly. Every Proxmox-specific call (clone,
# start, guest-agent address lookup, guest-agent key delivery, destroy) lives here, behind
# three functions, so a different provider (EC2, ...) only has to replace this one file —
# round-vm.sh itself never names a VM by anything but the address this returns
# (law-a-designation-is-never-derived would otherwise tempt a caller to guess one).
#
# Interface a provider must implement:
#   rvm_provider_provision  -> prints "<handle> <addr>" on stdout on success (0); on
#                              failure, prints the reason to stderr and returns 1.
#   rvm_provider_destroy <handle>
#   rvm_provider_alive <handle>   -> 0 if still running, 1 otherwise
#
# covers: spira/round-vm-provider-pve.sh
[ -n "${_RVM_PROVIDER_PVE_LOADED:-}" ] && return 0
_RVM_PROVIDER_PVE_LOADED=1

_RVM_PVE_ENV="${SPIRA_PVE_ENV:-${XDG_CONFIG_HOME:-$HOME/.config}/spira/pve.env}"
if [ -f "$_RVM_PVE_ENV" ]; then
    # shellcheck source=/dev/null
    . "$_RVM_PVE_ENV"
fi

_rvm_pve() { "${SPIRA_HOME:?}/pve.sh" "$@"; }

# _rvm_pve_addr_of <iface> — read an agent/network-get-interfaces JSON body from stdin,
# print the first IPv4 address on the named interface.
_rvm_pve_addr_of() {
    python3 -c "
import json, sys
iface = sys.argv[1]
try:
    data = json.load(sys.stdin)
except Exception:
    sys.exit(0)
for i in data.get('result', []) if isinstance(data, dict) else []:
    if i.get('name') != iface:
        continue
    for a in i.get('ip-addresses', []) or []:
        if a.get('ip-address-type') == 'ipv4':
            print(a.get('ip-address', ''))
            sys.exit(0)
" "$1"
}

# rvm_provider_provision -> "<vmid> <addr>". Clones the exact CI template (never a larger
# or different one — sp-msk4h's non-goals), starts it, waits for the guest agent to report
# a network address, then delivers the host's own public key into the VM's
# authorized_keys — the ONLY thing guest-agent delivers; every byte moved after this point
# (the round head, the batch results, the binaries) goes over plain ssh/rsync, addressed
# by nothing but the string this function returns.
rvm_provider_provision() {
    : "${PVE_TEMPLATE_VMID:?round-vm: PVE_TEMPLATE_VMID not set — is ${_RVM_PVE_ENV} present and readable?}"
    : "${SPIRA_ROUND_VM_HOST_PUBKEY:?round-vm: SPIRA_ROUND_VM_HOST_PUBKEY not set}"
    if [ ! -r "$SPIRA_ROUND_VM_HOST_PUBKEY" ]; then
        printf 'round-vm: SPIRA_ROUND_VM_HOST_PUBKEY not readable: %s\n' "$SPIRA_ROUND_VM_HOST_PUBKEY" >&2
        return 1
    fi

    local newid
    newid="$(_rvm_pve nextid 2>/dev/null)"
    if [ -z "$newid" ]; then
        printf 'round-vm: provision: API unreachable (nextid)\n' >&2
        return 1
    fi

    if ! _rvm_pve clone "$PVE_TEMPLATE_VMID" "$newid" "round-$newid" \
            ${PVE_RUNNER_POOL:+--pool "$PVE_RUNNER_POOL"} >&2; then
        printf 'round-vm: provision: clone refused\n' >&2
        return 1
    fi
    if ! _rvm_pve start "$newid" >&2; then
        printf 'round-vm: provision: VM did not start\n' >&2
        _rvm_pve destroy "$newid" --purge >/dev/null 2>&1 || true
        return 1
    fi

    local addr="" tries=0 max_tries="${SPIRA_ROUND_VM_BOOT_TRIES:-60}"
    while [ -z "$addr" ] && [ "$tries" -lt "$max_tries" ]; do
        addr="$(_rvm_pve guest-net "$newid" 2>/dev/null | _rvm_pve_addr_of "${SPIRA_ROUND_VM_NET_IFACE:-ens18}")"
        if [ -z "$addr" ]; then
            sleep "${SPIRA_ROUND_VM_BOOT_POLL:-2}"
            tries=$((tries + 1))
        fi
    done
    if [ -z "$addr" ]; then
        printf 'round-vm: provision: VM did not come up on the network\n' >&2
        _rvm_pve destroy "$newid" --purge >/dev/null 2>&1 || true
        return 1
    fi

    if ! _rvm_pve guest-file-write "$newid" ".ssh/authorized_keys" < "$SPIRA_ROUND_VM_HOST_PUBKEY" >&2; then
        printf 'round-vm: provision: guest-agent key delivery failed\n' >&2
        _rvm_pve destroy "$newid" --purge >/dev/null 2>&1 || true
        return 1
    fi

    printf '%s %s\n' "$newid" "$addr"
}

rvm_provider_destroy() {
    [ -n "${1:-}" ] || { printf 'round-vm: destroy: no handle given\n' >&2; return 1; }
    _rvm_pve destroy "$1" --purge
}

rvm_provider_alive() {
    [ -n "${1:-}" ] || return 1
    _rvm_pve status "$1" 2>/dev/null | grep -q '"status":[[:space:]]*"running"'
}
