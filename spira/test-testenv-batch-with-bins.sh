#!/usr/bin/env bash
# test-testenv-batch-with-bins.sh — --with-bins builds the branch's own Rust
# workspace and copies it into the branch worktree's bin/, so a suite that
# reads shipped binaries sees THIS branch's binaries rather than nothing (or
# a stale $REPO/bin left over from elsewhere).
#
# POSITIVE CONTROL (law-absence-needs-a-positive-control)
#   D1: without --with-bins, bin/widget is never built — the suite (which
#       asserts its presence) is red. Proves the suite can fail before a
#       green from D2 means anything.
#   D2: with --with-bins, the same fixture is green — bin/widget was built
#       and reached the container's checked-out tree.
#
# host-reason: drives testenv-batch.sh's own container and requires cargo on the host
# covers: spira/testenv-batch.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"

pass=0; fail=0
ok()      { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()     { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
iszero()  { [ "$2" = 0 ]    && ok "$1" || bad "$1" "expected 0, got $2"; }
isexit1() { [ "$2" = 1 ]    && ok "$1" || bad "$1" "expected 1, got $2"; }

BATCH="$HERE/testenv-batch.sh"
TESTENV="$HERE/testenv.sh"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

echo "test-testenv-batch-with-bins.sh"

# ---------------------------------------------------------------------------
# PRE-FLIGHT — skip if podman, user systemd, or cargo is unavailable.
# ---------------------------------------------------------------------------
command -v podman >/dev/null 2>&1 || {
    printf 'SKIP test-testenv-batch-with-bins.sh: podman not on PATH\n' >&2
    exit 77
}
command -v cargo >/dev/null 2>&1 || {
    printf 'SKIP test-testenv-batch-with-bins.sh: cargo not on PATH\n' >&2
    exit 77
}

PRE_CNAME="spira-batch-wbpre-$$"
bash "$TESTENV" up --name "$PRE_CNAME" >&2 || {
    printf 'SKIP test-testenv-batch-with-bins.sh: container did not start\n' >&2
    exit 77
}
if ! bash "$TESTENV" probe --name "$PRE_CNAME" 2>/dev/null; then
    bash "$TESTENV" down --name "$PRE_CNAME" >/dev/null 2>&1 || true
    printf 'SKIP test-testenv-batch-with-bins.sh: user systemd not available\n' >&2
    exit 77
fi
bash "$TESTENV" down --name "$PRE_CNAME" >/dev/null 2>&1 || true

# ---------------------------------------------------------------------------
# FIXTURE REPO — a minimal Cargo workspace with one bin crate (fast to build)
# plus a suite that is red unless bin/widget exists, is executable, and is
# non-empty. Checking the file inside the container's checked-out tree (not
# just on the host) proves --with-bins actually reaches the container, the
# same thing the pre-existing $REPO/bin copy step reaches.
# ---------------------------------------------------------------------------
REMOTE="$TMP/remote"
FIXTURE="$TMP/fixture"

git init -q --initial-branch=main "$REMOTE"
git -C "$REMOTE" config user.email "test@spira.local"
git -C "$REMOTE" config user.name "Spira Test"

mkdir -p "$REMOTE/widget/src" "$REMOTE/spira"
cat > "$REMOTE/Cargo.toml" << 'EOF'
[workspace]
members = ["widget"]
resolver = "2"
EOF
cat > "$REMOTE/widget/Cargo.toml" << 'EOF'
[package]
name = "widget"
version = "0.1.0"
edition = "2021"
EOF
cat > "$REMOTE/widget/src/main.rs" << 'EOF'
fn main() { println!("widget"); }
EOF
cat > "$REMOTE/Makefile" << 'EOF'
build:
	cargo build --release --workspace
EOF
cat > "$REMOTE/spira/test-target.sh" << 'SUITEEOF'
#!/usr/bin/env bash
set -uo pipefail
BIN="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)/bin/widget"
if [ -x "$BIN" ] && [ -s "$BIN" ]; then
    printf '  ok    test-target: bin/widget present and executable\n'; exit 0
else
    printf '  FAIL  test-target: bin/widget missing or empty\n'; exit 1
fi
SUITEEOF
chmod +x "$REMOTE/spira/test-target.sh"
git -C "$REMOTE" add -A
git -C "$REMOTE" commit -q -m "fixture: widget workspace + bin/-checking suite"

git clone -q --local "$REMOTE" "$FIXTURE"
git -C "$FIXTURE" config user.email "test@spira.local"
git -C "$FIXTURE" config user.name "Spira Test"

# Scratch CARGO_TARGET_DIR, isolated from the real host's incremental build
# cache — this fixture's crate is unrelated and should not share or pollute it.
BINS_TARGET_DIR="$TMP/cargo-target-bins"

echo
echo "D1: without --with-bins → rc=1 (bin/widget was never built)"

RESULTS_ROOT_D1="$TMP/results-D1"
rc_d1=0
SPIRA_BATCH_SKIP_INSTALL=1 \
SPIRA_BATCH_RESULTS="$RESULTS_ROOT_D1" \
SPIRA_BATCH_INSTANCE="wb-d1-$$" \
SPIRA_BATCH_BINS_TARGET_DIR="$BINS_TARGET_DIR" \
SPIRA_VERDICT_TTL=0 \
    bash "$BATCH" --suites test-target.sh main "$FIXTURE" || rc_d1=$?

isexit1 "D1: no --with-bins → bin/widget absent, suite red" "$rc_d1"

echo
echo "D2: --with-bins → rc=0 (bin/widget built into the branch worktree)"
echo "    (positive control: same fixture, same suite — only --with-bins differs)"

RESULTS_ROOT_D2="$TMP/results-D2"
rc_d2=0
SPIRA_BATCH_SKIP_INSTALL=1 \
SPIRA_BATCH_RESULTS="$RESULTS_ROOT_D2" \
SPIRA_BATCH_INSTANCE="wb-d2-$$" \
SPIRA_BATCH_BINS_TARGET_DIR="$BINS_TARGET_DIR" \
SPIRA_VERDICT_TTL=0 \
    bash "$BATCH" --with-bins --suites test-target.sh main "$FIXTURE" || rc_d2=$?

iszero "D2: --with-bins → bin/widget present, suite green" "$rc_d2"

echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ] || exit 1
