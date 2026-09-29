#!/usr/bin/env bash
#
# test-batcher-cut-round-vm.sh — batcher-cut's corpus step end to end through the REAL
# round-vm.sh (sp-o3o6z), against a stub Proxmox provider and a real container standing in
# for the round VM (the same stand-in test-round-vm-e2e.sh uses for round-vm.sh's own suite).
#
# WHAT THIS PROVES (the container tier of sp-o3o6z's acceptance): a batcher-cut round against
# the stub provider produces its verdict and binaries through round-vm.sh, not a direct
# testenv-batch.sh call, and the round result (round-vm.sh's own manifest) carries every
# measurement field — vm, acquire, vcpus, maxpar, batch wall, build wall and the sum of suite
# walls; land-local finds the binaries exactly where it reads them today
# (cargo-target-bins/<tree-sha>/release). test-batcher-cut.sh's own stub round-vm.sh already
# covers the wiring (argv, wall bound, exit 4) without a container — this suite is the one
# place the real round-vm.sh and a real (fixture) testenv-batch.sh run together.
#
# host-reason: needs podman, ssh, rsync and git on PATH, plus network egress to install
# openssh-server INSIDE the freshly started stand-in container — testenv-batch.sh's own
# container runs suites as an unprivileged user with none of those on PATH, so this suite
# SKIPS there; it runs for real wherever suites.sh's own cadence or an operator runs it
# directly, the same shape test-round-vm-e2e.sh already uses.
#
# defect: sp-o3o6z
# tier: T2
# covers: batcher-cut/src/*.rs spira/round-vm.sh
# timeout: 240
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
ROOT="$(cd "$HERE/.." && pwd -P)"
. "$HERE/testlib.sh"

echo "test-batcher-cut-round-vm.sh"

for _bin in podman ssh rsync git; do
    command -v "$_bin" >/dev/null 2>&1 || {
        printf 'SKIP test-batcher-cut-round-vm.sh: %s not found on PATH\n' "$_bin" >&2
        exit 77
    }
done

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-batcher-cut-round-vm
TMP="$(mktemp -d)"
VM_NAME="bcrv-e2e-$$"
cleanup() {
    podman rm -f "$VM_NAME" >/dev/null 2>&1
    [ -r "$TMP/state/git-daemon.pid" ] && kill "$(cat "$TMP/state/git-daemon.pid" 2>/dev/null)" >/dev/null 2>&1
    testdb_drop
    chmod -R u+w "$TMP" 2>/dev/null
    rm -rf "$TMP"
}
trap cleanup EXIT INT TERM
testdb_up batchercutrvm || { echo "test-batcher-cut-round-vm: could not build fixture database"; exit 1; }

CARGO_BIN="$(command -v cargo 2>/dev/null || true)"
[ -z "$CARGO_BIN" ] && [ -x "$HOME/.cargo/bin/cargo" ] && CARGO_BIN="$HOME/.cargo/bin/cargo"
[ -z "$CARGO_BIN" ] && [ -x "/usr/local/cargo/bin/cargo" ] && CARGO_BIN="/usr/local/cargo/bin/cargo"
if [ -z "$CARGO_BIN" ]; then
    echo "SKIP test-batcher-cut-round-vm: cargo not found — the batcher binary cannot be built"
    exit 77
fi
PATH="$(dirname "$CARGO_BIN"):$PATH"; export PATH
CARGO_TARGET_DIR_FOR_BUILD="$TMP/cargo-target"
BATCHER_BIN="$CARGO_TARGET_DIR_FOR_BUILD/release/batcher"
if [ ! -x "$BATCHER_BIN" ]; then
    printf '  (building batcher-cut into %s)\n' "$BATCHER_BIN"
    CARGO_TERM_COLOR=never CARGO_TARGET_DIR="$CARGO_TARGET_DIR_FOR_BUILD" \
        "$CARGO_BIN" build --release --manifest-path "$ROOT/Cargo.toml" -p batcher-cut 2>&1 | tail -10
fi
[ -x "$BATCHER_BIN" ] || { echo "test-batcher-cut-round-vm: batcher binary did not build"; exit 1; }

export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

# ---------------------------------------------------------------------------
# The "VM": a fresh container with none of round-vm.sh's tooling — sshd installed inside it,
# the same preparation step a real VM's provisioning would do. --network=host: the round
# path reaches it only by address, the same as a real VM's tailnet hostname.
# ---------------------------------------------------------------------------
IMG="docker.io/library/ubuntu:24.04"
podman run -d --name "$VM_NAME" --network=host --rm "$IMG" sleep 600 >/dev/null || \
    skip "podman could not start the stand-in container"

VM_PORT=$((21000 + (RANDOM % 10000)))
if ! podman exec "$VM_NAME" bash -c '
    export DEBIAN_FRONTEND=noninteractive
    apt-get -o Acquire::Retries=1 update -qq && \
    apt-get -o Acquire::Retries=1 install -y -qq --no-install-recommends openssh-server rsync git
' >"$TMP/apt.log" 2>&1; then
    skip "could not install openssh-server/rsync/git in the stand-in container (no network egress?) — $(tail -3 "$TMP/apt.log")"
fi

ssh-keygen -t ed25519 -N '' -q -f "$TMP/client_key"
podman exec "$VM_NAME" mkdir -p /root/.ssh /run/sshd
podman cp "$TMP/client_key.pub" "$VM_NAME:/root/.ssh/authorized_keys"
podman exec "$VM_NAME" bash -c 'chmod 700 /root/.ssh && chmod 600 /root/.ssh/authorized_keys'
podman exec -d "$VM_NAME" /usr/sbin/sshd -p "$VM_PORT" -D

deadline=$(( $(date +%s) + 15 ))
up=0
while [ "$(date +%s)" -lt "$deadline" ]; do
    if ssh -i "$TMP/client_key" -p "$VM_PORT" -o StrictHostKeyChecking=no \
           -o UserKnownHostsFile=/dev/null -o BatchMode=yes -o ConnectTimeout=2 \
           root@127.0.0.1 true 2>"$TMP/ssh-probe.log"; then
        up=1; break
    fi
    sleep 0.5
done
[ "$up" = 1 ] || skip "sshd never came up in the stand-in container ($(tail -3 "$TMP/ssh-probe.log" 2>/dev/null))"

# ---------------------------------------------------------------------------
# The fixture repo: queue.local so a green round lands with no forge in the picture at all —
# this suite is about round-vm.sh's own contract, not the PR/forge path test-batcher-cut.sh
# already covers. Its spira/testenv-batch.sh is a fixture honoring the SAME output contract
# the real one has (batch-results/, cargo-target-bins/<tree-sha>/release/, runner.meta with
# build_wall_s, tsd/*.jsonl) — testenv-batch.sh's own behavior is its own suites' job.
# ---------------------------------------------------------------------------
LREPO="$TMP/repo"
mkdir -p "$LREPO/spira"
cat > "$LREPO/spira/testenv-batch.sh" <<'FIXTURE'
#!/usr/bin/env bash
set -euo pipefail
SUITES=""
while [ $# -gt 0 ]; do
    case "$1" in
        --suites) SUITES="$2"; shift 2 ;;
        --mode|--with-bins) shift ;;
        *) shift ;;
    esac
done
REPO="$(cd "$(dirname "$0")/.." && pwd)"
RUN="$REPO/.runtime/spira"
TREE="$(git -C "$REPO" rev-parse HEAD^{tree})"
mkdir -p "$RUN/batch-results" "$RUN/cargo-target-bins/$TREE/release" "$RUN/tsd"
IFS=',' read -r -a suite_arr <<< "$SUITES"
for s in "${suite_arr[@]}"; do
    [ -n "$s" ] || continue
    printf 'green %s 2 - explicit 0\n' "$(date +%s)" > "$RUN/batch-results/$s.result"
    printf 'ok\n' > "$RUN/batch-results/$s.out"
done
printf 'nproc=4\nmemtotal_kb=100\nmaxpar=16\ncpu_busy_pct=10\nsuites_wall_s=1\nbuild_wall_s=9\n' \
    > "$RUN/batch-results/runner.meta"
printf '#!/bin/sh\necho fake-binary\n' > "$RUN/cargo-target-bins/$TREE/release/fakebin"
chmod +x "$RUN/cargo-target-bins/$TREE/release/fakebin"
python3 -c "
import json
print(json.dumps({'family':'suite-timing','run_id':'bcrv','branch':'round','suite':'combined','rc':0,'wall_secs':1,'mode':'parallel'}))
" >> "$RUN/tsd/suite-timing.jsonl"
FIXTURE
chmod +x "$LREPO/spira/testenv-batch.sh"
: > "$LREPO/spira/test-a.sh"
: > "$LREPO/spira/test-b.sh"
git -C "$LREPO" init --quiet -b trunk
git -C "$LREPO" add -A
git -C "$LREPO" commit --quiet -m base
git -C "$LREPO" branch local/main trunk

RUN="$TMP/run"; SH="$TMP/spira"
LANDSTATE="$RUN/landstate"; QUEUEDIR="$RUN/queue"
mkdir -p "$RUN/worktree" "$SH" "$LANDSTATE" "$QUEUEDIR/locland"
cp "$HERE"/*.sh "$HERE"/*.py "$SH/" 2>/dev/null
printf '#!/usr/bin/env bash\nexit 0\n' > "$SH/mail.sh"; chmod +x "$SH/mail.sh"
mkdir -p "$SH/chamber"
cp "$HERE/chamber/batcher.fayth" "$SH/chamber/"

REPONAME=locland
cat > "$SH/repo-map" <<RMAP
$REPONAME | $LREPO | queue.local | local/main | | |
RMAP

plant() {
    printf '{"id":"%s","title":"%s bead","status":"closed","issue_type":"task","labels":["spira","plan","repo:%s","express"],"updated_at":"2026-09-25T00:00:00Z"}\n' \
        "$1" "$1" "$REPONAME" | testdb_seed
}
certify() { printf 'CERTIFIED %s %s\n' "$2" "$(date +%s)" > "$LANDSTATE/$1"; }

testdb_reset
plant sp-bcrv1
git -C "$LREPO" worktree add -q -b spira/sp-bcrv1 "$RUN/worktree/sp-bcrv1" trunk
printf 'a\n' > "$RUN/worktree/sp-bcrv1/a.txt"
git -C "$RUN/worktree/sp-bcrv1" add -A
git -C "$RUN/worktree/sp-bcrv1" commit -q -m "sp-bcrv1: work"
tip="$(git -C "$LREPO" rev-parse spira/sp-bcrv1)"
git -C "$LREPO" worktree remove -f "$RUN/worktree/sp-bcrv1"
certify sp-bcrv1 "$tip"

# ---------------------------------------------------------------------------
# The stub provider: hands back the stand-in container's own address once — round-vm.sh's own
# contract under test, never a model of Proxmox (that is round-vm-provider-pve.sh's own
# suite, test-round-vm.sh).
# ---------------------------------------------------------------------------
FAKE_PROVIDER="$TMP/fake-provider.sh"
cat > "$FAKE_PROVIDER" <<SH
rvm_provider_provision() { printf '${VM_NAME} 127.0.0.1\n'; }
rvm_provider_destroy() { :; }
rvm_provider_alive() { return 1; }
SH

STATE_DIR="$TMP/rvm-state"
MIRROR_PORT=$((31000 + (RANDOM % 10000)))

cut_local() {
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" \
    SPIRA_BD="${SPIRA_BD:-$TESTDB_BD}" \
    SPIRA_REPO_MAP="$SH/repo-map" \
    SPIRA_QUEUE_DIR="$QUEUEDIR" \
    SPIRA_QUEUE_BATCH_WAIT=999999 \
    SPIRA_RELEASES="$TMP/local-releases" \
    SPIRA_TOML="$TMP/no-such.toml" \
    SPIRA_CONF="$TMP/no-such.conf" \
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
        "$BATCHER_BIN" cut "$REPONAME" --round-vm "$HERE/round-vm.sh" \
            --attribute "$SH/attribute.sh" 2>&1
}
mkdir -p "$TMP/local-releases"

out="$(cut_local)"
[ -n "$out" ] && printf '%s\n' "$out" | sed 's/^/  /'

want "cut: reports landing locally through round-vm.sh's own corpus" "landed locally" "$out"
head="$(printf '%s\n' "$out" | sed -n 's/.*landed locally at \([0-9a-f]\{7,\}\).*/\1/p' | head -1)"
is "cut: sp-bcrv1 landstate LANDED" "LANDED" "$(cut -d' ' -f1 < "$LANDSTATE/sp-bcrv1" 2>/dev/null)"

TREE_SHA="$(git -C "$LREPO" rev-parse "${head:-HEAD}^{tree}" 2>/dev/null)"
BIN="$RUN/cargo-target-bins/$TREE_SHA/release/fakebin"
[ -x "$BIN" ] \
    && ok "cut: the round's own binary landed exactly where land-local reads it (cargo-target-bins/<tree-sha>/release)" \
    || bad "cut: the round's own binary landed exactly where land-local reads it" "missing $BIN"

MANIFEST="$STATE_DIR/manifests/$TREE_SHA.json"
[ -r "$MANIFEST" ] && ok "cut: round-vm.sh wrote a round result for this tree sha" \
    || bad "cut: round-vm.sh wrote a round result for this tree sha" "missing $MANIFEST"
manifest_json="$(cat "$MANIFEST" 2>/dev/null)"
want "round result names the vm"      "\"vm\": \"$VM_NAME\""  "$manifest_json"
want "round result names acquire"     '"acquire": "cold"'     "$manifest_json"
want "round result names vcpus"       '"vcpus": 16'           "$manifest_json"
want "round result names maxpar"      '"maxpar": 16'          "$manifest_json"
want "round result names the batch wall"           '"batch_wall_secs":'      "$manifest_json"
want "round result names the build wall (fixture's own runner.meta)" '"build_wall_secs": 9' "$manifest_json"
want "round result names the sum of suite walls (2 suites x 2s)"     '"suite_wall_secs_sum": 4' "$manifest_json"

tl_summary
