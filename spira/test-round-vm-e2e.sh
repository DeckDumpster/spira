#!/usr/bin/env bash
#
# test-round-vm-e2e.sh — the container tier the round-vm crate's own Test strategy dropped
# (sp-o4wu7.3): the `round-vm` binary's `run` command, end to end, against a stub Proxmox
# HTTPS API served for real over a socket and a real container standing in for the VM.
# round-vm/DESIGN.md §4 only unit-tests against in-process fakes (a fake Provider, a fake
# Proxmox Transport, a fake Remote) — real ssh/rsync/git and the real HTTP wire format never
# run. That gap produced six live bugs (DESIGN.md §2.4) that no in-process fake could see: a
# wrong argv, a wrong HTTP body, a relative path the guest agent resolves somewhere sshd
# never reads. This suite runs the compiled binary against a real TLS server speaking the
# same request/response shapes and a real podman container reached over real ssh, so the
# same class of bug fails here before a live round finds it.
#
# THE PROXMOX STUB IS A STUB, NOT A MODEL OF PROXMOX: it answers next_id/clone/start/stop/
# destroy synchronously (no task polling — that loop is already covered by round-vm/src/
# pve.rs's own unit tests) and hands back the stand-in container's own address. But its
# agent/exec and agent/file-write calls are NOT faked — each one really execs inside the
# container (`podman exec`), so round-vm's actual key-delivery sequence (mkdir, file-write,
# chmod) has to land on a real filesystem before ssh will ever accept the connection this
# suite depends on.
#
# WHAT THIS PROVES: the mirror is fetched over the real read-only git-daemon round-vm itself
# starts; the stand-in VM is provisioned via the real HTTPS wire format (proven by requiring
# the two live-bug fields — file-write's absent `encoding`, exec's absent `capture-output` —
# to stay absent); the delivered key is the one that lets real ssh in; a two-suite batch runs
# and its results, tsd row and a fake binary come back over real rsync; the fake binary lands
# exactly where `queue land-local` reads it; the manifest names the real vmid, vcpus and
# maxpar; and the VM is destroyed and verified gone by vmid.
#
# THE POOL-OF-ONE PRE-WARMS: `run`'s acquire hands out vmid N and — by design (DESIGN.md
# G1) — starts a real, detached `round-vm _provision-bg` for the NEXT vmid before returning.
# That process outlives this suite's own `run` invocation, so the final "is N gone" check is
# scoped to the vmid this run actually leased, not to the whole VM list.
#
# host-reason: needs podman, ssh, rsync, git, openssl and a Rust toolchain on PATH, plus
# network egress to install openssh-server/rsync/git INSIDE the freshly started container —
# testenv-batch.sh's own container runs suites as an unprivileged user with none of that on
# PATH, so this suite SKIPs there; it runs for real wherever suites.sh's own cadence or an
# operator runs it directly (the same shape test-batch-owner.sh's "host-reason: needs podman
# on PATH" already uses).
#
# tier: T3
# covers: round-vm/src/*.rs
# timeout: 300
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
ROOT="$(cd "$HERE/.." && pwd -P)"
. "$HERE/testlib.sh"

echo "test-round-vm-e2e.sh"

for _bin in podman ssh rsync git openssl python3; do
    command -v "$_bin" >/dev/null 2>&1 || skip "$_bin not found on PATH"
done
PATH="$PATH"; export PATH

TMP="$(mktemp -d)"
VM_NAME="rvm-e2e-$$"
STUB_PID=""
cleanup() {
    [ -n "$STUB_PID" ] && kill "$STUB_PID" >/dev/null 2>&1
    if [ -r "$TMP/state/pool.json" ]; then
        bg_pid="$(python3 -c '
import json,sys
try:
    s=json.load(open(sys.argv[1]))
    p=s.get("provisioning")
    print(p["owner"]["pid"] if p else "")
except Exception:
    print("")
' "$TMP/state/pool.json" 2>/dev/null)"
        [ -n "$bg_pid" ] && kill "$bg_pid" >/dev/null 2>&1
    fi
    podman rm -f "$VM_NAME" >/dev/null 2>&1
    [ -r "$TMP/state/git-daemon.pid" ] && kill "$(cat "$TMP/state/git-daemon.pid" 2>/dev/null)" >/dev/null 2>&1
    chmod -R u+w "$TMP" 2>/dev/null
    rm -rf "$TMP"
}
trap cleanup EXIT INT TERM

# ── the tree's round-vm, by name on PATH (law-absence-needs-a-positive-control: no binary,
# no suite).
BIN="$(command -v round-vm 2>/dev/null || true)"
[ -n "$BIN" ] || bail "round-vm is not on PATH (the tree's build provides it)"

# ── the "VM": a fresh container with none of round-vm's tooling — sshd is installed INSIDE
# it, the same preparation a real VM's provisioning would already have done, not baked into
# an image ahead of time. authorized_keys is deliberately NOT placed here: round-vm's own
# key delivery (through the stub's real podman-exec-backed agent/exec, agent/file-write) has
# to be what makes ssh work, or this suite would prove nothing about that path.
IMG="docker.io/library/ubuntu:24.04"
podman run -d --name "$VM_NAME" --network=host --rm "$IMG" sleep 900 >/dev/null || skip "podman could not start the stand-in container"

SSH_PORT=$((20000 + (RANDOM % 10000)))
MIRROR_PORT=$((30000 + (RANDOM % 10000)))
PVE_PORT=$((40000 + (RANDOM % 10000)))

if ! podman exec "$VM_NAME" bash -c '
    export DEBIAN_FRONTEND=noninteractive
    apt-get -o Acquire::Retries=1 update -qq && \
    apt-get -o Acquire::Retries=1 install -y -qq --no-install-recommends openssh-server rsync git
' >"$TMP/apt.log" 2>&1; then
    skip "could not install openssh-server/rsync/git in the stand-in container (no network egress?) — $(tail -3 "$TMP/apt.log")"
fi
podman exec "$VM_NAME" mkdir -p /run/sshd
podman exec -d "$VM_NAME" /usr/sbin/sshd -p "$SSH_PORT" -D

ssh-keygen -t ed25519 -N '' -q -f "$TMP/client_key"

# The round's own testenv is faked the same way its output contract is faked in every other
# round-vm suite: a `cargo` on the VM's PATH that never builds anything, just writes the
# files round-vm's REMOTE_SCRIPT (run.rs) pulls back — this is round-vm's OWN contract under
# test, not testenv's, which is covered by testenv's own suites.
cat > "$TMP/fake-cargo" <<'FAKECARGO'
#!/bin/sh
set -eu
mkdir -p .runtime/spira/batch-results .runtime/spira/tsd target/release
TREE="$(git rev-parse HEAD^{tree})"
printf 'green %s 30 - explicit 0\n' "$(date +%s)" > .runtime/spira/batch-results/e2e-a.sh.result
printf 'green %s 12 - explicit 0\n' "$(date +%s)" > .runtime/spira/batch-results/e2e-b.sh.result
printf 'key=KEY\ntree=%s\n' "$TREE" > .runtime/spira/batch-results/batch.meta
printf 'build_wall_s=5\n' > .runtime/spira/batch-results/runner.meta
printf '{"family":"suite-timing","suite":"combined","rc":0}\n' > .runtime/spira/tsd/suite-timing.jsonl
printf '#!/bin/sh\necho fake-binary\n' > target/release/fakebin
chmod +x target/release/fakebin
FAKECARGO
chmod +x "$TMP/fake-cargo"
podman cp "$TMP/fake-cargo" "$VM_NAME:/usr/local/bin/cargo"
podman exec "$VM_NAME" chmod +x /usr/local/bin/cargo

# ── the stub Proxmox HTTPS API: a CA + a leaf cert it presents, so PVE_CACERT (the CA) is
# usable as a rustls trust root the way HttpTransport::new expects — a bare self-signed leaf
# added directly as a root fails rustls's own chain-building (CaUsedAsEndEntity).
{
    openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 \
        -keyout "$TMP/ca_key.pem" -out "$TMP/ca_cert.pem" -days 1 -nodes \
        -subj "/CN=round-vm-e2e-ca" -addext "basicConstraints=critical,CA:TRUE" \
        -addext "keyUsage=critical,keyCertSign,cRLSign"
    openssl req -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 \
        -keyout "$TMP/pve_key.pem" -out "$TMP/pve.csr" -nodes -subj "/CN=127.0.0.1"
    openssl x509 -req -in "$TMP/pve.csr" -CA "$TMP/ca_cert.pem" -CAkey "$TMP/ca_key.pem" \
        -CAcreateserial -out "$TMP/pve_cert.pem" -days 1 \
        -extfile <(printf 'subjectAltName=IP:127.0.0.1\nbasicConstraints=CA:FALSE\nkeyUsage=digitalSignature,keyEncipherment')
} >"$TMP/openssl.log" 2>&1 || skip "could not generate the stub's TLS certificate — $(tail -5 "$TMP/openssl.log")"

REQLOG="$TMP/reqlog.jsonl"
: > "$REQLOG"
python3 "$HERE/round-vm-stub-pve.py" "$PVE_PORT" "$VM_NAME" "$TMP/pve_cert.pem" "$TMP/pve_key.pem" "$REQLOG" &
STUB_PID=$!
for _ in $(seq 1 20); do
    python3 -c "import socket; socket.create_connection(('127.0.0.1', $PVE_PORT), timeout=1).close()" 2>/dev/null && break
    sleep 0.25
done

cat > "$TMP/pve.env" <<EOF
PVE_API_HOST=127.0.0.1
PVE_API_PORT=$PVE_PORT
PVE_NODE=pve
PVE_TOKEN_ID=testid
PVE_TOKEN_SECRET=testsecret
PVE_CACERT=$TMP/ca_cert.pem
PVE_TEMPLATE_VMID=9000
EOF

# ── the fixture tree: round-vm's own mirror/clone contract is under test, not its content.
TREE_DIR="$TMP/tree"
mkdir -p "$TREE_DIR"
echo "round-vm e2e fixture" > "$TREE_DIR/README"
git -C "$TREE_DIR" init --quiet -b main
git -C "$TREE_DIR" -c user.email=t@example.com -c user.name=t add -A
git -C "$TREE_DIR" -c user.email=t@example.com -c user.name=t commit --quiet -m fixture
TREE_SHA="$(git -C "$TREE_DIR" rev-parse HEAD^{tree})"

STATE_DIR="$TMP/state"
RUN_DIR="$TMP/run"
rc=0
tl_config SPIRA_RUN="$RUN_DIR" SPIRA_ROUND_VM_STATE_DIR="$STATE_DIR" SPIRA_PVE_ENV="$TMP/pve.env" \
    SPIRA_ROUND_VM_SSH_USER=root SPIRA_ROUND_VM_SSH_PORT="$SSH_PORT" \
    SPIRA_ROUND_VM_HOST_KEY="$TMP/client_key" SPIRA_ROUND_VM_HOST_ADDR=127.0.0.1 \
    SPIRA_ROUND_VM_MIRROR_PORT="$MIRROR_PORT" SPIRA_ROUND_VM_MAX_RETRIES=1 \
    SPIRA_ROUND_VM_RETRY_INTERVAL=0 SPIRA_ROUND_VM_VCPUS=4
env -i PATH="$PATH" \
    SPIRA_TOML="$SPIRA_TOML" \
    SPIRA_ROUND_VM_SSH_TRIES=20 \
    SPIRA_ROUND_VM_BOOT_POLL=1 \
    "$BIN" run "$TREE_DIR" --suites e2e-a.sh,e2e-b.sh --maxpar 2 \
    >"$TMP/run.out" 2>"$TMP/run.err" || rc=$?

is "run: exits 0" "0" "$rc"
[ "$rc" -ne 0 ] && { cat "$TMP/run.out"; cat "$TMP/run.err"; }

# The vmid round-vm actually leased for this run — read back from its own stderr line
# ("round-vm run: <handle> <addr> <mode>"), not assumed, since the stub hands out the next
# free id rather than a fixed one.
VMID="$(awk '{print $3}' "$TMP/run.err" | head -1)"
want "run: named the leased vmid on stderr" "round-vm run: " "$(cat "$TMP/run.err")"

want "run: batch results for suite A landed" "e2e-a.sh.result" "$(ls "$RUN_DIR/batch-results" 2>/dev/null)"
want "run: batch results for suite B landed" "e2e-b.sh.result" "$(ls "$RUN_DIR/batch-results" 2>/dev/null)"

LAND_BIN="$TREE_DIR/target/release/fakebin"
[ -x "$LAND_BIN" ] && ok "run: the fake binary landed executable under the round worktree" \
    || bad "run: the fake binary landed executable under the round worktree" "missing $LAND_BIN"
is "run: lands exactly where queue land-local's own bins_dir computation reads it" \
    "$TREE_DIR/target/release" "$(dirname "$LAND_BIN")"

MANIFEST="$STATE_DIR/manifests/$TREE_SHA.json"
[ -r "$MANIFEST" ] && ok "run: wrote a manifest for this tree sha" || bad "run: wrote a manifest for this tree sha" "missing $MANIFEST"
manifest_json="$(cat "$MANIFEST" 2>/dev/null)"
want "run: manifest names the leased vm" "\"vm\":\"$VMID\"" "$manifest_json"
want "run: manifest names acquire=cold (no warm VM existed yet)" "\"acquire\":\"cold\"" "$manifest_json"
want "run: manifest names the configured vcpus (non-default: 4)" "\"vcpus\":4" "$manifest_json"
want "run: manifest names the requested maxpar" "\"maxpar\":2" "$manifest_json"
want "run: manifest names the tree sha" "\"tree_sha\":\"$TREE_SHA\"" "$manifest_json"
want "run: manifest's tree_sha_found matches — the VM's testenv reported the right tree" "\"tree_sha_found\":\"$TREE_SHA\"" "$manifest_json"
want "run: manifest sums the two suites' own wall times (30s + 12s)" "\"suite_wall_secs_sum\":42" "$manifest_json"
want "run: manifest names the build wall the fixture's runner.meta reported" "\"build_wall_secs\":5" "$manifest_json"

tsd_line="$(cat "$RUN_DIR/tsd/suite-timing.jsonl" 2>/dev/null)"
want "run: the VM's tsd row was merged into the host's run/tsd" "\"suite\":\"combined\"" "$tsd_line"
want "run: the merged tsd row is tagged with ran_on the leased vmid" "\"ran_on\":\"$VMID\"" "$tsd_line"
want "run: the merged tsd row is tagged with vcpus" "\"vcpus\":4" "$tsd_line"
want "run: the merged tsd row is tagged with maxpar" "\"maxpar\":2" "$tsd_line"

# ── the live-bug wire-format assertions (round-vm/DESIGN.md §2.4), against the REAL HTTP
# bodies the stub logged — not a unit test's in-process call, the actual bytes ureq sent.
want "positive control: the reqlog really captured file-write's params" "\"content\"" "$(cat "$REQLOG")"
want "positive control: the reqlog really captured exec's params" "\"command\"" "$(cat "$REQLOG")"
nowant "run: file-write sent no encoding param (live bug 2)" "\"encoding\"" "$(cat "$REQLOG")"
nowant "run: exec sent no capture-output param" "\"capture-output\"" "$(cat "$REQLOG")"

# ── key delivery actually happened on the real filesystem, not merely reported success.
DELIVERED_KEY="$(podman exec "$VM_NAME" cat /root/.ssh/authorized_keys 2>/dev/null)"
EXPECTED_KEY="$(cat "$TMP/client_key.pub")"
is "run: the delivered authorized_keys is exactly the host's public key" "$EXPECTED_KEY" "$DELIVERED_KEY"
KEY_MODE="$(podman exec "$VM_NAME" stat -c %a /root/.ssh/authorized_keys 2>/dev/null)"
is "run: authorized_keys was chmod 600 (absolute path, live bug 1)" "600" "$KEY_MODE"

# ── the VM this run leased is destroyed and verified gone; a background pre-warm for the
# NEXT vmid (DESIGN.md G1) may legitimately still be listed, so this checks by vmid.
qemu_list="$(python3 -c "
import ssl, urllib.request
ctx = ssl.create_default_context()
ctx.check_hostname = False
ctx.verify_mode = ssl.CERT_NONE
print(urllib.request.urlopen('https://127.0.0.1:$PVE_PORT/api2/json/nodes/pve/qemu', context=ctx, timeout=5).read().decode())
" 2>/dev/null)"
nowant "run: the leased VM was destroyed and verified gone" "\"vmid\": $VMID" "$qemu_list"

tl_summary
