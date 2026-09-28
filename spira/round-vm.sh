#!/usr/bin/env bash
# round-vm.sh — a pool of one ephemeral round VM: acquire / release / run / status.
#
#   round-vm.sh acquire
#       Prints "<handle> <addr> <warm|cold>" and returns the ready VM. Starts exactly one
#       background provision of the next VM before returning. If no VM is ready, provisions
#       one synchronously (acquire=cold). If the provider cannot produce a VM at all, waits
#       and retries every SPIRA_ROUND_VM_RETRY_INTERVAL seconds, alarming ONCE per outage —
#       never falls back to running anywhere else.
#
#   round-vm.sh release <handle>
#       Destroys the named VM. Nothing is reused, so nothing needs to be cleaned first.
#
#   round-vm.sh run <tree-dir> [--suites <csv>] [--maxpar <n>]
#       acquire -> the VM fetches <tree-dir>'s HEAD from a read-only mirror this command
#       maintains -> runs testenv-batch.sh --mode parallel --with-bins on the VM -> pulls
#       batch-results/, cargo-target-bins/<tree-sha>/release/ and tsd/*.jsonl back with
#       rsync -> writes a manifest naming the tree sha, vm, acquire mode, vcpus, maxpar and
#       wall time -> refuses to install binaries whose tree sha does not match <tree-dir>'s
#       own HEAD^{tree} -> release.
#
#   round-vm.sh status
#       Reports the pool: the ready VM if any, a background provision in flight, the last
#       alarmed outage if one is still open.
#
# PROVIDER SEAM. Every Proxmox-specific call (clone, start, guest-agent address lookup and
# key delivery, destroy) lives behind SPIRA_ROUND_VM_PROVIDER (default:
# round-vm-provider-pve.sh, reusing pve.sh). Everything past that point — the address this
# returns, ssh, rsync, the git fetch — is provider-agnostic: the round path reaches the VM
# only by that address, never by another Proxmox call.
#
# NO LOCAL MODE. An unreachable provider makes acquire (and so `run`) wait and retry; it
# never runs the batch anywhere but the VM. law-a-control-that-cannot-check-must-refuse.
#
# covers: spira/round-vm.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/conf.sh"

: "${SPIRA_ROUND_VM_STATE_DIR:=${SPIRA_RUN:?}/round-vm}"
: "${SPIRA_ROUND_VM_PROVIDER:=$HERE/round-vm-provider-pve.sh}"
: "${SPIRA_ROUND_VM_SSH_USER:=root}"
: "${SPIRA_ROUND_VM_SSH_PORT:=22}"
: "${SPIRA_ROUND_VM_HOST_KEY:=$SPIRA_ROUND_VM_STATE_DIR/host_key}"
: "${SPIRA_ROUND_VM_HOST_PUBKEY:=$SPIRA_ROUND_VM_HOST_KEY.pub}"
: "${SPIRA_ROUND_VM_VCPUS:=16}"
: "${SPIRA_ROUND_VM_MAXPAR:=16}"
: "${SPIRA_ROUND_VM_RETRY_INTERVAL:=60}"
: "${SPIRA_ROUND_VM_MAX_RETRIES:=0}"   # 0 = retry forever (production); tests bound this
: "${SPIRA_ROUND_VM_MIRROR_PORT:=9430}"
: "${SPIRA_ROUND_VM_MAIL_MAILBOX:=${SPIRA_MAIL_SESSION_MAILBOX:-concierge}}"

STATE_DIR="$SPIRA_ROUND_VM_STATE_DIR"
SELF="$HERE/round-vm.sh"

_rvm_load_provider() {
    [ -r "$SPIRA_ROUND_VM_PROVIDER" ] || {
        printf 'round-vm.sh: SPIRA_ROUND_VM_PROVIDER not readable: %s\n' "$SPIRA_ROUND_VM_PROVIDER" >&2
        return 1
    }
    # shellcheck source=/dev/null
    . "$SPIRA_ROUND_VM_PROVIDER"
    for _fn in rvm_provider_provision rvm_provider_destroy rvm_provider_alive; do
        if ! command -v "$_fn" >/dev/null 2>&1; then
            printf 'round-vm.sh: provider %s does not define %s\n' "$SPIRA_ROUND_VM_PROVIDER" "$_fn" >&2
            return 1
        fi
    done
}

# _rvm_alarm <reason> — mails SPIRA_ROUND_VM_MAIL_MAILBOX once per distinct reason, tracked
# in $STATE_DIR/alarm-outage; a repeated call for the SAME reason (a steady outage, polled
# every retry interval) is silent, exactly the divergence-alarm's own discipline
# (queue_local_check_divergence, lib.sh). Cleared by _rvm_alarm_clear the moment acquire
# next succeeds, so a later, different outage alarms anew.
_rvm_alarm() {
    local reason="$1" marker="$STATE_DIR/alarm-outage" prev=""
    [ -r "$marker" ] && prev="$(cat "$marker" 2>/dev/null)"
    [ "$prev" = "$reason" ] && return 0
    mkdir -p "$STATE_DIR" 2>/dev/null || true
    printf '%s\n' "$reason" > "$marker"
    [ -x "$SPIRA_HOME/mail.sh" ] || return 0
    printf '## Alert\nround-vm: %s\n\nThe round has not started; retrying every %ss.\n' \
        "$reason" "$SPIRA_ROUND_VM_RETRY_INTERVAL" \
    | "$SPIRA_HOME/mail.sh" send "$SPIRA_ROUND_VM_MAIL_MAILBOX" \
        --from "Round VM <round-vm@spira>" \
        --subject "round VM outage: $reason" \
        --kind alert >/dev/null 2>&1 || true
}

_rvm_alarm_clear() {
    rm -f "$STATE_DIR/alarm-outage" 2>/dev/null || true
}

# _rvm_provision_with_retry -> "<handle> <addr>" on stdout. Retries forever
# (SPIRA_ROUND_VM_MAX_RETRIES=0) unless bounded for a test, alarming once per distinct
# failure reason and never returning a "run locally" sentinel of any kind.
_rvm_provision_with_retry() {
    local attempts=0 out reason
    while :; do
        if out="$(rvm_provider_provision 2>"$STATE_DIR/.provision-err.$$")"; then
            rm -f "$STATE_DIR/.provision-err.$$"
            printf '%s\n' "$out"
            return 0
        fi
        reason="$(tail -1 "$STATE_DIR/.provision-err.$$" 2>/dev/null)"
        rm -f "$STATE_DIR/.provision-err.$$"
        [ -n "$reason" ] || reason="provider failed with no reason given"
        _rvm_alarm "$reason"
        attempts=$((attempts + 1))
        if [ "${SPIRA_ROUND_VM_MAX_RETRIES}" -gt 0 ] && [ "$attempts" -ge "${SPIRA_ROUND_VM_MAX_RETRIES}" ]; then
            printf 'round-vm.sh: acquire: giving up after %s attempt(s): %s\n' "$attempts" "$reason" >&2
            return 1
        fi
        sleep "$SPIRA_ROUND_VM_RETRY_INTERVAL"
    done
}

_rvm_background_provision_next() {
    mkdir -p "$STATE_DIR" 2>/dev/null || true
    if [ -r "$STATE_DIR/provisioning.pid" ]; then
        local pid; pid="$(cat "$STATE_DIR/provisioning.pid" 2>/dev/null)"
        if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then
            return 0
        fi
        rm -f "$STATE_DIR/provisioning.pid"
    fi
    [ -r "$STATE_DIR/ready" ] && return 0

    if command -v setsid >/dev/null 2>&1; then
        setsid bash "$SELF" _internal-provision-bg \
            </dev/null >"$STATE_DIR/provisioning.out" 2>&1 &
    else
        bash "$SELF" _internal-provision-bg \
            </dev/null >"$STATE_DIR/provisioning.out" 2>&1 &
    fi
    printf '%s\n' "$!" > "$STATE_DIR/provisioning.pid"
}

cmd__internal_provision_bg() {
    _rvm_load_provider || { rm -f "$STATE_DIR/provisioning.pid"; return 1; }
    local out=""
    if out="$(rvm_provider_provision)"; then
        printf '%s\n' "$out" > "$STATE_DIR/ready.tmp" && mv -f "$STATE_DIR/ready.tmp" "$STATE_DIR/ready"
    fi
    rm -f "$STATE_DIR/provisioning.pid"
}

cmd_acquire() {
    _rvm_load_provider || return 1
    mkdir -p "$STATE_DIR" 2>/dev/null || true
    exec 9>"$STATE_DIR/lock"
    flock 9

    if [ -r "$STATE_DIR/provisioning.pid" ]; then
        local pid; pid="$(cat "$STATE_DIR/provisioning.pid" 2>/dev/null)"
        [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null || rm -f "$STATE_DIR/provisioning.pid"
    fi

    if [ -r "$STATE_DIR/ready" ]; then
        local line handle addr
        line="$(cat "$STATE_DIR/ready")"
        handle="${line%% *}"; addr="${line#* }"
        if [ -n "$handle" ] && [ -n "$addr" ] && rvm_provider_alive "$handle"; then
            rm -f "$STATE_DIR/ready"
            _rvm_alarm_clear
            printf '%s %s warm\n' "$handle" "$addr"
            _rvm_background_provision_next
            return 0
        fi
        rm -f "$STATE_DIR/ready"
    fi

    local acquired
    acquired="$(_rvm_provision_with_retry)" || return 1
    _rvm_alarm_clear
    printf '%s cold\n' "$acquired"
    _rvm_background_provision_next
}

cmd_release() {
    [ $# -ge 1 ] || { printf 'round-vm.sh release: usage: round-vm.sh release <handle>\n' >&2; return 2; }
    _rvm_load_provider || return 1
    rvm_provider_destroy "$1"
}

cmd_status() {
    mkdir -p "$STATE_DIR" 2>/dev/null || true
    if [ -r "$STATE_DIR/ready" ]; then
        printf 'ready: %s\n' "$(cat "$STATE_DIR/ready")"
    else
        printf 'ready: none\n'
    fi
    if [ -r "$STATE_DIR/provisioning.pid" ] && kill -0 "$(cat "$STATE_DIR/provisioning.pid" 2>/dev/null)" 2>/dev/null; then
        printf 'provisioning: pid %s\n' "$(cat "$STATE_DIR/provisioning.pid")"
    else
        printf 'provisioning: none\n'
    fi
    if [ -r "$STATE_DIR/alarm-outage" ]; then
        printf 'outage: %s\n' "$(cat "$STATE_DIR/alarm-outage")"
    else
        printf 'outage: none\n'
    fi
}

# _rvm_mirror_update <mirror-dir> <tree-dir> — fast-forwards the persistent bare mirror's
# "round" branch to <tree-dir>'s HEAD, and points the mirror's HEAD at it so a plain `git
# clone` of the mirror checks out exactly that commit on a named branch (never detached —
# testenv-batch.sh's landref resolution needs a symbolic HEAD; see its rung 4).
_rvm_mirror_update() {
    local mirror="$1" tree_dir="$2"
    [ -d "$mirror" ] || git init --quiet --bare "$mirror" || return 1
    git -C "$tree_dir" rev-parse HEAD >/dev/null 2>&1 || return 1
    git --git-dir="$mirror" fetch --quiet "$tree_dir" "+HEAD:refs/heads/round" || return 1
    git --git-dir="$mirror" symbolic-ref HEAD refs/heads/round
}

# _rvm_git_daemon_ensure <state-dir> <port> — a read-only, anonymous git-daemon serving
# everything under <state-dir> (so the mirror at <state-dir>/mirror.git resolves at
# git://<addr>:<port>/mirror.git). Nothing is copied that git cannot verify: the VM's own
# `git clone` checks the objects it receives the same way any git client does.
_rvm_git_daemon_ensure() {
    local state_dir="$1" port="$2" pidfile="$state_dir/git-daemon.pid"
    if [ -r "$pidfile" ]; then
        local pid; pid="$(cat "$pidfile" 2>/dev/null)"
        if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then
            return 0
        fi
        rm -f "$pidfile"
    fi
    git daemon --reuseaddr --listen=0.0.0.0 --port="$port" \
        --base-path="$state_dir" --export-all --pid-file="$pidfile" --detach \
        >/dev/null 2>&1
    sleep 1
    [ -r "$pidfile" ] && kill -0 "$(cat "$pidfile" 2>/dev/null)" 2>/dev/null
}

# _rvm_merge_tsd <pulled-tsd-dir> <vm> <vcpus> <maxpar> — appends the VM's run/tsd rows into
# this host's own run/tsd, tagging each with ran_on/vcpus/maxpar so a suite-timing query can
# tell a VM-run row from a host-run one. Same append-only, dedup-by-line, flock discipline as
# tsd-ingest.sh's own merge, because two producers writing the same family file is exactly
# the race that discipline exists for.
_rvm_merge_tsd() {
    local src_dir="$1" vm="$2" vcpus="$3" maxpar="$4" f family dest
    [ -d "$src_dir" ] || return 0
    for f in "$src_dir"/*.jsonl; do
        [ -e "$f" ] || continue
        family="$(basename "$f" .jsonl)"
        dest="${SPIRA_RUN:?}/tsd/$family.jsonl"
        mkdir -p "$(dirname "$dest")"
        (
            exec 200>"$dest.lock"
            flock -x 200
            local tmp_seen; tmp_seen="$(mktemp)"
            [ -f "$dest" ] && cp "$dest" "$tmp_seen" || : > "$tmp_seen"
            while IFS= read -r line; do
                [ -n "$line" ] || continue
                line="$(printf '%s' "$line" | python3 -c "
import json, sys
row = json.load(sys.stdin)
row['ran_on'] = sys.argv[1]
row['vcpus'] = int(sys.argv[2])
row['maxpar'] = int(sys.argv[3])
print(json.dumps(row))
" "$vm" "$vcpus" "$maxpar" 2>/dev/null)" || continue
                grep -qxF "$line" "$tmp_seen" 2>/dev/null && continue
                printf '%s\n' "$line" >> "$dest"
                printf '%s\n' "$line" >> "$tmp_seen"
            done < "$f"
            rm -f "$tmp_seen"
        )
    done
}

_rvm_ssh() {
    ssh -i "$SPIRA_ROUND_VM_HOST_KEY" -p "$SPIRA_ROUND_VM_SSH_PORT" \
        -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o BatchMode=yes \
        -o ConnectTimeout=10 "$@"
}

_rvm_rsync_pull() {
    local addr="$1" remote_path="$2" local_dir="$3"
    mkdir -p "$local_dir" 2>/dev/null || true
    rsync -a -e "ssh -i $SPIRA_ROUND_VM_HOST_KEY -p $SPIRA_ROUND_VM_SSH_PORT -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o BatchMode=yes -o ConnectTimeout=10" \
        "${SPIRA_ROUND_VM_SSH_USER}@${addr}:${remote_path}" "$local_dir/" 2>/dev/null
}

cmd_run() {
    [ $# -ge 1 ] || {
        printf 'round-vm.sh run: usage: round-vm.sh run <tree-dir> [--suites <csv>] [--maxpar <n>]\n' >&2
        return 2
    }
    local tree_dir="$1" suites="" maxpar="$SPIRA_ROUND_VM_MAXPAR"
    shift
    while [ $# -gt 0 ]; do
        case "$1" in
            --suites) suites="${2:-}"; shift 2 ;;
            --suites=*) suites="${1#--suites=}"; shift ;;
            --maxpar) maxpar="${2:-}"; shift 2 ;;
            --maxpar=*) maxpar="${1#--maxpar=}"; shift ;;
            *) printf 'round-vm.sh run: unknown option: %s\n' "$1" >&2; return 2 ;;
        esac
    done

    git -C "$tree_dir" rev-parse --git-dir >/dev/null 2>&1 || {
        printf 'round-vm.sh run: not a git checkout: %s\n' "$tree_dir" >&2
        return 2
    }
    : "${SPIRA_ROUND_VM_HOST_ADDR:?round-vm.sh run: SPIRA_ROUND_VM_HOST_ADDR not set}"
    if [ ! -r "$SPIRA_ROUND_VM_HOST_KEY" ]; then
        printf 'round-vm.sh run: SPIRA_ROUND_VM_HOST_KEY not readable: %s\n' "$SPIRA_ROUND_VM_HOST_KEY" >&2
        return 2
    fi

    local sha tree_sha
    sha="$(git -C "$tree_dir" rev-parse HEAD)" || return 1
    tree_sha="$(git -C "$tree_dir" rev-parse "HEAD^{tree}")" || return 1

    local mirror="$STATE_DIR/mirror.git"
    mkdir -p "$STATE_DIR" 2>/dev/null || true
    _rvm_mirror_update "$mirror" "$tree_dir" || {
        printf 'round-vm.sh run: cannot update the mirror from %s\n' "$tree_dir" >&2
        return 1
    }
    _rvm_git_daemon_ensure "$STATE_DIR" "$SPIRA_ROUND_VM_MIRROR_PORT" || {
        printf 'round-vm.sh run: cannot start the mirror git-daemon on port %s\n' "$SPIRA_ROUND_VM_MIRROR_PORT" >&2
        return 1
    }

    local acq_line handle addr acquire_mode
    acq_line="$(cmd_acquire)" || {
        printf 'round-vm.sh run: acquire failed\n' >&2
        return 1
    }
    read -r handle addr acquire_mode <<<"$acq_line"

    local t0 wall remote_rc=0
    t0=$(date +%s)
    _rvm_ssh "${SPIRA_ROUND_VM_SSH_USER}@${addr}" bash -s -- \
        "$SPIRA_ROUND_VM_HOST_ADDR" "$SPIRA_ROUND_VM_MIRROR_PORT" "$suites" "$maxpar" <<'REMOTE' || remote_rc=$?
set -euo pipefail
host_addr="$1" port="$2" suites="$3" maxpar="$4"
rm -rf ~/round-work
git clone --quiet "git://${host_addr}:${port}/mirror.git" ~/round-work
cd ~/round-work
export SPIRA_BATCH_MAXPAR="$maxpar"
if [ -n "$suites" ]; then
    exec bash spira/testenv-batch.sh --mode parallel --with-bins --suites "$suites" round
else
    exec bash spira/testenv-batch.sh --mode parallel --with-bins round
fi
REMOTE
    wall=$(( $(date +%s) - t0 ))

    _rvm_rsync_pull "$addr" "round-work/.runtime/spira/batch-results/" "${SPIRA_RUN:?}/batch-results"

    local pulled_tsd="$STATE_DIR/.pulled-tsd.$$"
    rm -rf "$pulled_tsd"
    _rvm_rsync_pull "$addr" "round-work/.runtime/spira/tsd/" "$pulled_tsd"
    _rvm_merge_tsd "$pulled_tsd" "$handle" "$SPIRA_ROUND_VM_VCPUS" "$maxpar"
    rm -rf "$pulled_tsd"

    local pulled_bins="$STATE_DIR/.pulled-bins.$$"
    rm -rf "$pulled_bins"
    _rvm_rsync_pull "$addr" "round-work/.runtime/spira/cargo-target-bins/" "$pulled_bins"

    local found_tree=""
    [ -d "$pulled_bins" ] && found_tree="$(cd "$pulled_bins" 2>/dev/null && ls -1 2>/dev/null | head -1)"

    mkdir -p "$STATE_DIR/manifests"
    _rvm_write_manifest "$STATE_DIR/manifests/$tree_sha.json" \
        "$tree_sha" "$found_tree" "$sha" "$handle" "$acquire_mode" "$SPIRA_ROUND_VM_VCPUS" "$maxpar" "$wall"

    local rc=0
    _rvm_install_bins "$pulled_bins" "$tree_sha" "${SPIRA_BATCH_BINS_TARGET_DIR:-$SPIRA_RUN/cargo-target-bins}" || rc=1
    rm -rf "$pulled_bins"

    rvm_provider_destroy "$handle" >/dev/null 2>&1 \
        || printf 'round-vm.sh run: warning: release of %s failed\n' "$handle" >&2

    [ "$remote_rc" -eq 0 ] || rc=1
    return "$rc"
}

# _rvm_write_manifest <path> <tree-sha> <tree-sha-found> <commit-sha> <vm> <acquire> <vcpus> <maxpar> <wall-secs>
_rvm_write_manifest() {
    local path="$1"; shift
    python3 -c "
import json, sys
d = dict(tree_sha=sys.argv[1], tree_sha_found=(sys.argv[2] or None), commit_sha=sys.argv[3],
         vm=sys.argv[4], acquire=sys.argv[5], vcpus=int(sys.argv[6]), maxpar=int(sys.argv[7]),
         wall_secs=int(sys.argv[8]))
json.dump(d, open(sys.argv[9], 'w'))
" "$1" "$2" "$3" "$4" "$5" "$6" "$7" "$8" "$path"
}

# _rvm_install_bins <pulled-bins-dir> <expected-tree-sha> <bins-target-base> — the only
# place a --with-bins directory keyed by tree sha (testenv-batch.sh's own convention:
# cargo-target-bins/<tree-sha>/release) is trusted onto this host. <pulled-bins-dir> holds
# whatever the VM itself named its own tree-sha directory as; a name that does not match
# <expected-tree-sha> means the VM built something other than the commit this host asked
# for (a stale run, a race with acquire handing out a warm VM mid-build), and nothing is
# installed. Absence of any directory at all (a batch that never reached --with-bins) is
# not a refusal — there is nothing to refuse.
_rvm_install_bins() {
    local pulled="$1" expected="$2" target_base="$3" found=""
    [ -d "$pulled" ] && found="$(cd "$pulled" && ls -1 2>/dev/null | head -1)"
    [ -n "$found" ] || return 0
    if [ "$found" != "$expected" ] || [ ! -d "$pulled/$found/release" ]; then
        printf 'round-vm.sh run: refusing binaries: tree sha %s does not match expected %s — nothing installed\n' \
            "$found" "$expected" >&2
        return 1
    fi
    mkdir -p "$target_base/$expected"
    rsync -a "$pulled/$found/release/" "$target_base/$expected/release/"
}

# Sourceable as a library (test-round-vm.sh exercises internal functions like
# _rvm_install_bins directly, without the fetch/ssh/rsync machinery around them) as well as
# executable directly — the dispatch below runs only when this file is the process itself.
if [ "${BASH_SOURCE[0]}" = "${0}" ]; then
    [ $# -ge 1 ] || {
        printf 'usage: round-vm.sh acquire|release <handle>|run <tree-dir> [--suites <csv>] [--maxpar <n>]|status\n' >&2
        exit 1
    }
    verb="$1"; shift
    case "$verb" in
        acquire)                 cmd_acquire "$@" ;;
        release)                 cmd_release "$@" ;;
        run)                     cmd_run "$@" ;;
        status)                  cmd_status "$@" ;;
        _internal-provision-bg)  cmd__internal_provision_bg "$@" ;;
        *)
            printf 'round-vm.sh: unknown verb: %s\n' "$verb" >&2
            exit 1
            ;;
    esac
fi
