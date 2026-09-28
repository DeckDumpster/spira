#!/usr/bin/env bash
#
# test-round-vm-e2e.sh — round-vm.sh's `run` command, end to end, against a stub Proxmox
# provider and a real, SSH-reachable process standing in for the VM.
#
#   ./test-round-vm-e2e.sh
#
# WHAT THIS PROVES (the container tier of sp-7tw9h's Test strategy): fetch the round head
# over the read-only git-daemon mirror, run a two-suite batch on the "VM", pull the batch
# results, tsd rows and a fake binary back over real ssh/rsync, and land the binary exactly
# where queue.sh's land-local reads it from (cargo-target-bins/<tree-sha>/release).
#
# THE "VM" IS A SECOND SSHD, NOT A NESTED CONTAINER: testenv-batch.sh already provides the
# container this suite runs in, and round-vm.sh's own host side (ssh, rsync, git daemon)
# needs openssh/rsync installed in THIS process's environment regardless of what stands in
# for the VM — a nested podman container would need the exact same packages installed a
# second time, for no more fidelity than a second sshd on a second port. round-vm.sh only
# ever addresses the VM by hostname:port, so it cannot tell the difference.
#
# THE PROVIDER IS A STUB, NOT A MODEL OF PROXMOX: it hands back this sshd's own address —
# the real Proxmox provider (round-vm-provider-pve.sh) is exercised for its argument and
# credential checks in test-round-vm.sh; nothing here pretends to be Proxmox's API.
#
# NEEDS openssh-client, openssh-server, rsync and git (with git-daemon) on PATH. The
# testenv image does not carry these by default — installed here, once, with network
# egress; skip cleanly rather than fail if that is not available.
#
# defect: sp-7tw9h
# tier: T2
# covers: spira/round-vm.sh
# timeout: 120
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

printf 'test-round-vm-e2e.sh\n'

_have() { command -v "$1" >/dev/null 2>&1; }

if ! { _have ssh && _have sshd && _have rsync && _have git && [ -x /usr/lib/git-core/git-daemon ]; }; then
    export DEBIAN_FRONTEND=noninteractive
    apt-get -o Acquire::Retries=1 update -qq >/tmp/rvm-e2e-apt.log 2>&1 \
        && apt-get -o Acquire::Retries=1 install -y -qq --no-install-recommends \
               openssh-client openssh-server rsync >>/tmp/rvm-e2e-apt.log 2>&1
fi
if ! { _have ssh && _have sshd && _have rsync && _have git && [ -x /usr/lib/git-core/git-daemon ]; }; then
    skip "openssh/rsync/git-daemon not available and could not be installed (no network egress?) — see /tmp/rvm-e2e-apt.log"
fi

TMP="$(mktemp -d)"
SSHD_PID="" GITD_PID=""
cleanup() {
    [ -n "$SSHD_PID" ] && kill "$SSHD_PID" >/dev/null 2>&1
    [ -r "$TMP/state/git-daemon.pid" ] && kill "$(cat "$TMP/state/git-daemon.pid" 2>/dev/null)" >/dev/null 2>&1
    rm -rf "$TMP"
}
trap cleanup EXIT INT TERM

# ---------------------------------------------------------------------------
# The "VM": a real sshd, reachable only at 127.0.0.1:$VM_PORT — never touching the box's
# own /etc/ssh, own users, or own authorized_keys.
# ---------------------------------------------------------------------------
VM_PORT=$((20000 + (RANDOM % 10000)))
mkdir -p "$TMP/vm/ssh" "$TMP/vm/priv"
ssh-keygen -t ed25519 -N '' -q -f "$TMP/vm/ssh/host_key"
ssh-keygen -t ed25519 -N '' -q -f "$TMP/client_key"
cat > "$TMP/vm/authorized_keys" <<KEYS
$(cat "$TMP/client_key.pub")
KEYS
mkdir -p /run/sshd 2>/dev/null || true
cat > "$TMP/vm/sshd_config" <<CONF
Port $VM_PORT
ListenAddress 127.0.0.1
HostKey $TMP/vm/ssh/host_key
PidFile $TMP/vm/sshd.pid
AuthorizedKeysFile $TMP/vm/authorized_keys
PermitRootLogin yes
PasswordAuthentication no
KbdInteractiveAuthentication no
UsePAM no
StrictModes no
PrintMotd no
Subsystem sftp /usr/lib/openssh/sftp-server
CONF

/usr/sbin/sshd -f "$TMP/vm/sshd_config" -D -e >"$TMP/vm/sshd.log" 2>&1 &
SSHD_PID=$!
deadline=$(( $(date +%s) + 10 ))
up=0
while [ "$(date +%s)" -lt "$deadline" ]; do
    if ssh -i "$TMP/client_key" -p "$VM_PORT" -o StrictHostKeyChecking=no \
           -o UserKnownHostsFile=/dev/null -o BatchMode=yes -o ConnectTimeout=2 \
           root@127.0.0.1 true 2>>"$TMP/vm/sshd.log"; then
        up=1; break
    fi
    sleep 0.3
done
[ "$up" = 1 ] || skip "the stand-in sshd never came up ($(tail -3 "$TMP/vm/sshd.log" 2>/dev/null))"

# ---------------------------------------------------------------------------
# The fixture tree: a throwaway repo whose spira/testenv-batch.sh is a stand-in honoring
# the SAME output contract the real one has (batch-results/, cargo-target-bins/<tree
# sha>/release/, tsd/*.jsonl) — testenv-batch.sh's own behavior is covered by its own
# suites; this is round-vm.sh's contract under test, not testenv-batch.sh's.
# ---------------------------------------------------------------------------
TREE_DIR="$TMP/tree"
mkdir -p "$TREE_DIR/spira"
cat > "$TREE_DIR/spira/testenv-batch.sh" <<'FIXTURE'
#!/usr/bin/env bash
set -euo pipefail
MODE="" SUITES="" WITH_BINS=0 BR=""
while [ $# -gt 0 ]; do
    case "$1" in
        --mode) MODE="$2"; shift 2 ;;
        --with-bins) WITH_BINS=1; shift ;;
        --suites) SUITES="$2"; shift 2 ;;
        *) BR="$1"; shift ;;
    esac
done
REPO="$(cd "$(dirname "$0")/.." && pwd)"
RUN="$REPO/.runtime/spira"
TREE="$(git -C "$REPO" rev-parse HEAD^{tree})"
mkdir -p "$RUN/batch-results" "$RUN/cargo-target-bins/$TREE/release" "$RUN/tsd"
IFS=',' read -r -a suite_arr <<< "$SUITES"
for s in "${suite_arr[@]}"; do
    [ -n "$s" ] || continue
    printf 'green %s 1 0.10 - explicit 0\n' "$(date +%s)" > "$RUN/batch-results/$s.result"
    printf 'ok\n' > "$RUN/batch-results/$s.out"
done
printf '#!/bin/sh\necho fake-binary\n' > "$RUN/cargo-target-bins/$TREE/release/fakebin"
chmod +x "$RUN/cargo-target-bins/$TREE/release/fakebin"
python3 -c "
import json
print(json.dumps({'family':'suite-timing','run_id':'e2e','branch':'$BR','suite':'combined','rc':0,'wall_secs':1,'mode':'$MODE'}))
" >> "$RUN/tsd/suite-timing.jsonl"
FIXTURE
chmod +x "$TREE_DIR/spira/testenv-batch.sh"
git -C "$TREE_DIR" init --quiet -b main
git -C "$TREE_DIR" -c user.email=t@example.com -c user.name=t add -A
git -C "$TREE_DIR" -c user.email=t@example.com -c user.name=t commit --quiet -m fixture
TREE_SHA="$(git -C "$TREE_DIR" rev-parse HEAD^{tree})"

# ---------------------------------------------------------------------------
# The stub provider: hands back the stand-in sshd's own address once.
# ---------------------------------------------------------------------------
FAKE_HOME="$TMP/fake-home"
mkdir -p "$FAKE_HOME"
cat > "$FAKE_HOME/mail.sh" <<'SH'
#!/usr/bin/env bash
exit 0
SH
chmod +x "$FAKE_HOME/mail.sh"

FAKE_PROVIDER="$TMP/fake-provider.sh"
cat > "$FAKE_PROVIDER" <<SH
rvm_provider_provision() {
    printf 'e2e-vm 127.0.0.1\n'
}
rvm_provider_destroy() { :; }
rvm_provider_alive() { return 1; }
SH

HOST_RUN="$TMP/hostrun"
STATE_DIR="$TMP/state"
MIRROR_PORT=$((30000 + (RANDOM % 10000)))

rc=0
env -i PATH="$PATH" HOME="$TMP" \
    SPIRA_HOME="$FAKE_HOME" \
    SPIRA_TOML="$TMP/no-such.toml" SPIRA_CONF="$TMP/no-such.conf" \
    SPIRA_RUN="$HOST_RUN" \
    SPIRA_ROUND_VM_STATE_DIR="$STATE_DIR" \
    SPIRA_ROUND_VM_PROVIDER="$FAKE_PROVIDER" \
    SPIRA_ROUND_VM_SSH_USER=root \
    SPIRA_ROUND_VM_SSH_PORT="$VM_PORT" \
    SPIRA_ROUND_VM_HOST_KEY="$TMP/client_key" \
    SPIRA_ROUND_VM_HOST_ADDR=127.0.0.1 \
    SPIRA_ROUND_VM_MIRROR_PORT="$MIRROR_PORT" \
    SPIRA_ROUND_VM_MAX_RETRIES=1 \
    SPIRA_ROUND_VM_RETRY_INTERVAL=0 \
    SPIRA_ROUND_VM_VCPUS=16 \
    bash "$HERE/round-vm.sh" run "$TREE_DIR" --suites e2e-a.sh,e2e-b.sh --maxpar 2 \
    >"$TMP/run.out" 2>"$TMP/run.err" || rc=$?

[ -r "$STATE_DIR/git-daemon.pid" ] && GITD_PID="$(cat "$STATE_DIR/git-daemon.pid" 2>/dev/null)"

is "run: exits 0" "0" "$rc"
[ "$rc" -ne 0 ] && { cat "$TMP/run.out" "$TMP/run.err"; }

want "run: batch results for both suites landed" "e2e-a.sh.result" "$(ls "$HOST_RUN/batch-results" 2>/dev/null)"
want "run: batch results for both suites landed" "e2e-b.sh.result" "$(ls "$HOST_RUN/batch-results" 2>/dev/null)"

BIN="$HOST_RUN/cargo-target-bins/$TREE_SHA/release/fakebin"
[ -x "$BIN" ] \
    && ok "run: the fake binary landed at cargo-target-bins/<tree-sha>/release" \
    || bad "run: the fake binary landed at cargo-target-bins/<tree-sha>/release" "missing $BIN"

# The exact path queue.sh's _land_local_bins_dir computes: SPIRA_RUN/cargo-target-bins/<head's
# tree sha>/release. Recomputed here the same way (tree sha of HEAD), not copied from a
# constant, so a change to that convention would break this assertion instead of agreeing
# with it by coincidence.
LAND_LOCAL_BINS_DIR="$HOST_RUN/cargo-target-bins/$(git -C "$TREE_DIR" rev-parse HEAD^{tree})/release"
is "run: lands exactly where land-local's own bins_dir computation reads it" \
    "$LAND_LOCAL_BINS_DIR" "$(dirname "$BIN")"

MANIFEST="$STATE_DIR/manifests/$TREE_SHA.json"
[ -r "$MANIFEST" ] && ok "run: wrote a manifest for this tree sha" \
    || bad "run: wrote a manifest for this tree sha" "missing $MANIFEST"
manifest_json="$(cat "$MANIFEST" 2>/dev/null)"
want "run: manifest names the vm" "e2e-vm" "$manifest_json"
want "run: manifest names acquire=cold (the only VM there was)" '"acquire": "cold"' "$manifest_json"
want "run: manifest names the configured vcpus" '"vcpus": 16' "$manifest_json"
want "run: manifest names the requested maxpar" '"maxpar": 2' "$manifest_json"
want "run: manifest names the tree sha" "$TREE_SHA" "$manifest_json"

tsd_line="$(cat "$HOST_RUN/tsd/suite-timing.jsonl" 2>/dev/null)"
want "run: the VM's tsd row was merged into the host's run/tsd" "combined" "$tsd_line"
want "run: the merged tsd row is tagged with ran_on" '"ran_on": "e2e-vm"' "$tsd_line"
want "run: the merged tsd row is tagged with vcpus" '"vcpus": 16' "$tsd_line"
want "run: the merged tsd row is tagged with maxpar" '"maxpar": 2' "$tsd_line"

tl_summary
