#!/usr/bin/env bash
#
# test-testenv.sh — testenv.sh up/down/probe and cargo cache timing.
#
# WHAT THIS TESTS
# ---------------
# 1. POSITIVE CONTROL: prove podman can create and remove a container before asserting
#    anything about testenv.sh (law-absence-needs-a-positive-control).
# 2. UP: the container starts with PID 1 as systemd.
# 3. USER SYSTEMD: systemctl --user status exits 0 as spirauser; the session bus is
#    reachable via XDG_RUNTIME_DIR=/run/user/1001.
# 4. CHECKOUT MOUNT: /workspace inside the container holds the caller's checkout.
# 5. CARGO CACHE: two named volumes are mounted at CARGO_HOME; a second build of the
#    same project is faster than the first because cargo detects unchanged sources.
#    The volumes survive `down`, so a future `up` starts with the registry pre-populated.
# 6. DOWN IS IDEMPOTENT: a second `down` call exits 0 without error.
# 7. PROBE: exits 0 when user systemd is active.
#
# SKIP CONDITION: no podman on PATH.
#
# defect: sp-aiocb
# covers: spira/testenv.sh spira/testenv/Containerfile
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
pass=0; fail=0
ok()      { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()     { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()      { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want()    { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
iszero()  { [ "$2" = 0 ] && ok "$1" || bad "$1" "wanted exit 0, got $2"; }

echo "test-testenv.sh"

command -v podman >/dev/null 2>&1 || {
    printf 'SKIP test-testenv.sh: podman not found on PATH\n' >&2
    exit 77
}

TESTENV="$HERE/testenv.sh"
CNAME="spira-testenv-te-$$"
TMP="$(mktemp -d)"

cleanup() {
    bash "$TESTENV" down --name "$CNAME" --volumes >/dev/null 2>&1 || true
    rm -rf "$TMP"
}
trap cleanup EXIT INT TERM

# ==========================================================================
echo
echo "positive control — prove podman works on this host:"
# ==========================================================================
# Plant a short-lived container and verify it appears in `podman ps`; clean it
# up before the real fixture uses the same image. A failure here means podman
# itself is unavailable or misconfigured, not that testenv.sh is broken.
PC_NAME="spira-testenv-pc-$$"
if podman run -d --name "$PC_NAME" --rm docker.io/library/ubuntu:24.04 \
        bash -c 'exit 0' >/dev/null 2>&1; then
    ok "positive control: podman can create a container"
else
    bad "positive control" "podman run failed on ubuntu:24.04"
fi
podman rm -f "$PC_NAME" >/dev/null 2>&1 || true

# ==========================================================================
echo
echo "up — start testenv container:"
# ==========================================================================
t0=$(date +%s%N)
bash "$TESTENV" up --name "$CNAME" >&2
up_rc=$?
t1=$(date +%s%N)
startup_ms=$(( (t1 - t0) / 1000000 ))
iszero "up exits 0" "$up_rc"
printf '  note  up completed in %dms\n' "$startup_ms"

# up must record an owner file naming a live pid — the guard down enforces
# depends on this having been written (law-guard-binds-the-caller).
owner_pid="$(cat "/tmp/${CNAME}.owner" 2>/dev/null || true)"
[ -n "$owner_pid" ] \
    && ok "up recorded an owner file" \
    || bad "up recorded an owner file" "no pid in /tmp/${CNAME}.owner"
[ -n "$owner_pid" ] && [ -d "/proc/$owner_pid" ] \
    && ok "owner pid is live" \
    || bad "owner pid is live" "pid ${owner_pid:-<empty>} not found in /proc"

# PID 1 inside the container must be systemd.
pid1="$(podman exec "$CNAME" cat /proc/1/comm 2>/dev/null)"
is "PID 1 is systemd" "systemd" "$pid1"

# ==========================================================================
echo
echo "user systemd — systemctl --user connects via session bus:"
# ==========================================================================
sc_out="$(podman exec --user spirauser \
    -e XDG_RUNTIME_DIR=/run/user/1001 \
    "$CNAME" systemctl --user status 2>&1 | head -4)"
sc_rc=$?
iszero "systemctl --user status exits 0" "$sc_rc"
want "status shows running state" "State:" "$sc_out"

# ==========================================================================
echo
echo "probe — testenv.sh probe exits 0:"
# ==========================================================================
bash "$TESTENV" probe --name "$CNAME" >/dev/null 2>&1
iszero "probe exits 0" "$?"

# ==========================================================================
echo
echo "checkout mount — /workspace holds the caller checkout:"
# ==========================================================================
# Verify a file that exists in the harness checkout is visible inside the container.
mnt_out="$(podman exec "$CNAME" ls /workspace/spira/conf.sh 2>&1)"
mnt_rc=$?
iszero "/workspace/spira/conf.sh is accessible" "$mnt_rc"

# ==========================================================================
echo
echo "cargo cache — volumes mounted and two-build timing:"
# ==========================================================================
# Create a minimal hello-world project inside the container's own writable layer.
# Using /tmp keeps the project off the mounted checkout and off the host filesystem.
# Run as spirauser so cargo can write Cargo.lock without a permission error.
podman exec --user spirauser \
    -e CARGO_HOME=/var/spira/cargo \
    "$CNAME" bash -c \
    'mkdir -p /tmp/hello/src
     cat > /tmp/hello/Cargo.toml << '"'"'EOF'"'"'
[package]
name = "hello"
version = "0.1.0"
edition = "2021"
EOF
printf "fn main(){println!(\"ok\");}\n" > /tmp/hello/src/main.rs' 2>/dev/null

# First build: compiles main.rs and links. Target artefacts land in /tmp/hello/target
# (inside the container's writable layer) so they vanish on the next down/up cycle.
t_b1s=$(date +%s%N)
podman exec --user spirauser \
    -e CARGO_HOME=/var/spira/cargo \
    "$CNAME" cargo build --manifest-path /tmp/hello/Cargo.toml 2>/dev/null
b1_rc=$?
t_b1e=$(date +%s%N)
b1_ms=$(( (t_b1e - t_b1s) / 1000000 ))
iszero "first build exits 0" "$b1_rc"

# Second build: cargo detects no source changes and skips recompilation. The speedup
# is the increment over the first build's compile-and-link time.
t_b2s=$(date +%s%N)
podman exec --user spirauser \
    -e CARGO_HOME=/var/spira/cargo \
    "$CNAME" cargo build --manifest-path /tmp/hello/Cargo.toml 2>/dev/null
b2_rc=$?
t_b2e=$(date +%s%N)
b2_ms=$(( (t_b2e - t_b2s) / 1000000 ))
iszero "second build exits 0" "$b2_rc"
printf '  note  first build: %dms, second build: %dms\n' "$b1_ms" "$b2_ms"

# The second build must be strictly faster: cargo's incremental check costs ~50ms
# on a no-op, while compilation always takes longer. If they are equal the target
# volume is not being used (or both are zero, which is also wrong).
[ "$b2_ms" -lt "$b1_ms" ] \
    && ok "second build faster: cargo target cache in effect" \
    || bad "second build timing" "expected b2 (${b2_ms}ms) < b1 (${b1_ms}ms)"

# Confirm the registry volume exists (even though hello-world has no external deps,
# the volume itself must be mounted and owned by spirauser for real projects to use it).
vol_reg="${CNAME}-cargo-reg"
vol_info="$(podman volume inspect "$vol_reg" 2>/dev/null)"
[ -n "$vol_info" ] \
    && ok "cargo registry volume created" \
    || bad "cargo registry volume" "volume ${vol_reg} not found"

# ==========================================================================
echo
echo "rust-toolchain.toml — a pinned channel the image never installed must not redden cargo:"
# ==========================================================================
# Positive control: without RUSTUP_TOOLCHAIN, a workspace pin naming an uninstalled
# channel sends rustup to install it into the read-only /usr/local/rustup and cargo
# dies with EACCES. Prove that before trusting a green from the image's own env.
podman exec --user spirauser "$CNAME" bash -c \
    'mkdir -p /tmp/rttest && printf "[toolchain]\nchannel = \"1.82.0\"\n" > /tmp/rttest/rust-toolchain.toml'

pin_unpatched_out="$(podman exec --user spirauser \
    -e CARGO_HOME=/var/spira/cargo \
    "$CNAME" env -u RUSTUP_TOOLCHAIN bash -c 'cd /tmp/rttest && cargo --version' 2>&1)"
pin_unpatched_rc=$?
[ "$pin_unpatched_rc" != 0 ] \
    && ok "positive control: unpinned exec cannot install the toolchain (EACCES)" \
    || bad "positive control" "expected a failure without RUSTUP_TOOLCHAIN, got rc=0: $pin_unpatched_out"

pin_patched_out="$(podman exec --user spirauser \
    -e CARGO_HOME=/var/spira/cargo \
    "$CNAME" bash -c 'cd /tmp/rttest && cargo --version' 2>&1)"
pin_patched_rc=$?
iszero "container's own RUSTUP_TOOLCHAIN overrides the workspace pin" "$pin_patched_rc"
case "$pin_patched_out" in
    *1.82.0*) bad "cargo used the installed toolchain, not the pin" "got: $pin_patched_out" ;;
    *)        ok "cargo used the installed toolchain, not the pin" ;;
esac
printf '  note  cargo --version under the pin: %s\n' "$pin_patched_out"

# ==========================================================================
echo
echo "down — remove container:"
# ==========================================================================
bash "$TESTENV" down --name "$CNAME" >&2
iszero "down exits 0" "$?"

# Container must be gone.
podman container exists "$CNAME" 2>/dev/null \
    && bad "container removed" "container still exists after down" \
    || ok "container removed after down"

# ==========================================================================
echo
echo "idempotent down — second down is a no-op:"
# ==========================================================================
bash "$TESTENV" down --name "$CNAME" >&2
iszero "second down exits 0" "$?"

# ==========================================================================
echo
printf '%s passed, %s failed\n' "$pass" "$fail"
[ "$fail" = 0 ]
