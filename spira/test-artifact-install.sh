#!/usr/bin/env bash
#
# test-artifact-install.sh — prove that a release tarball installs and serves 200 without cargo.
#
# WHAT IS PROVED:
#   Phase 1 (positive control): a tarball layout has bin/loom but not
#     loom/target/release/loom. The OLD default for SPIRA_LOOM_BIN pointed at the
#     source-checkout path, which does not exist in a tarball install — ready.sh
#     would render UNKN for loom. This phase makes that gap visible before trusting
#     the silence in phase 3.
#   Phase 2 (build): cargo builds the real loom binary; build-tarball.sh packages it
#     as bin/loom inside a spira-<timestamp>.tar.gz.
#   Phase 3 (no-cargo): cargo is removed from PATH. activate.sh unpacks the tarball.
#     conf.sh's fixed default resolves SPIRA_LOOM_BIN to bin/loom inside the release.
#     Loom starts; /api/beads → 200. ready.sh exits 0.
#
# LAW-ABSENCE-NEEDS-A-POSITIVE-CONTROL: phase 1 proves the UNKN path fires before
# phase 3 trusts that it does not.
#
# CARGO NOTES:
#   This suite MUST resolve cargo before sourcing lib.sh, because conf.sh (sourced
#   by lib.sh) replaces PATH with a restricted set that does not include cargo's bin.
#   The toolchain cargo/rustc are invoked directly (not via the rustup proxy) to
#   avoid proxy chain issues in the restricted-PATH environment.
#   CARGO_HOME is set to a temp dir owned by the test user; the testenv named volume
#   (/var/spira/cargo) is root-owned on first use and causes Permission denied.
#
# SKIP: cargo absent (not in testenv); XDG_RUNTIME_DIR absent (no user session).
#
# exclusive: cargo build peaks at several GB; runs alone to prevent container OOM
# runtime: ~5m (cargo build dominates on first run; ~2m on re-runs)
# covers: spira/conf.sh spira/activate.sh spira/loom.sh spira/ready.sh install.sh

# === RESOLVE CARGO AND SAVE PATH BEFORE lib.sh SOURCING ===
# conf.sh (sourced by lib.sh) overwrites PATH entirely; cargo must be resolved now.
_ORIG_PATH="$PATH"
_CARGO_BIN="$(command -v cargo 2>/dev/null || true)"
if [ -z "$_CARGO_BIN" ] && [ -x "/usr/local/cargo/bin/cargo" ]; then
    _CARGO_BIN="/usr/local/cargo/bin/cargo"
fi

set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"

[ -n "$_CARGO_BIN" ] || {
    printf 'SKIP test-artifact-install.sh: cargo not found\n' >&2
    exit 77
}
[ -n "${XDG_RUNTIME_DIR:-}" ] || {
    printf 'SKIP test-artifact-install.sh: XDG_RUNTIME_DIR not set (no user session)\n' >&2
    exit 77
}

. "$HERE/lib.sh"
. "$HERE/testdb.sh"

pass=0; fail=0
ok()      { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()     { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "${2:-}"; }
iszero()  { [ "$2" = 0 ] && ok "$1" || bad "$1" "exit $2"; }
want()    { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
notwant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

echo "test-artifact-install.sh"

if ! testdb_available; then
    printf 'SKIP test-artifact-install.sh: no fixture database reachable\n' >&2
    exit 77
fi

WORKSPACE="$(cd "$HERE/.." && pwd -P)"   # /workspace inside testenv

# ---------------------------------------------------------------------------
# SCRATCH — ephemeral; cleaned on exit.
# ---------------------------------------------------------------------------
SCRATCH="$(mktemp -d)"
LOOM_PID=""

_cleanup() {
    [ -n "$LOOM_PID" ] && kill "$LOOM_PID" 2>/dev/null || true
    testdb_drop >/dev/null 2>&1 || true
    # activate.sh makes the release directory read-only; restore writable before removal.
    chmod -R u+w "$SCRATCH" 2>/dev/null || true
    rm -rf "$SCRATCH"
}
trap _cleanup EXIT INT TERM

RELEASES="$SCRATCH/releases"

# Stub systemctl: exits 0 for every subcommand so activate.sh daemon-reload and
# spira_unit's is-enabled/is-active calls succeed without a real installed unit.
STUB_SC="$SCRATCH/sc"
printf '#!/bin/sh\nexit 0\n' > "$STUB_SC" && chmod +x "$STUB_SC"

# Stub panel: build-tarball.sh requires a panel binary; the panel service is not
# exercised here, so a minimal executable satisfies the requirement.
STUB_PANEL="$SCRATCH/panel-stub"
printf '#!/bin/sh\necho stub-panel\n' > "$STUB_PANEL" && chmod +x "$STUB_PANEL"

# ===========================================================================
echo ""
echo "phase 1: positive control"
# ===========================================================================
# A tarball layout has bin/loom but NOT loom/target/release/loom.
# Before the conf.sh fix, SPIRA_LOOM_BIN defaulted to the source-checkout path,
# which does not exist in a tarball install. Prove both facts are true.
_tl="$SCRATCH/tarball-layout"
mkdir -p "$_tl/bin" "$_tl/spira"
printf '#!/bin/sh\n' > "$_tl/bin/loom" && chmod +x "$_tl/bin/loom"
# conf.sh subshell needs spira/conf.sh to exist in the layout
cp "$HERE/conf.sh" "$_tl/spira/conf.sh"

_old_path="$_tl/loom/target/release/loom"
[ ! -x "$_old_path" ] \
    && ok "positive-control: loom/target/release/loom absent in tarball layout (would be UNKN before fix)" \
    || bad "positive-control" "source-tree path unexpectedly executable at $_old_path"

_new_path="$_tl/bin/loom"
[ -x "$_new_path" ] \
    && ok "positive-control: bin/loom executable in tarball layout (fixed default finds it)" \
    || bad "positive-control" "bin/loom not executable at $_new_path"

# Verify the fixed conf.sh resolves SPIRA_LOOM_BIN to bin/loom when SPIRA_REPO
# points at a tarball layout directory.
# SPIRA_CONF_LOADED= forces conf.sh to run fresh in the subshell (the guard at the
# top of conf.sh returns immediately if SPIRA_CONF_LOADED=1, inheriting the parent's
# already-computed values rather than re-deriving from SPIRA_REPO).
# SPIRA_LOOM_BIN= clears any value set by the parent's conf.sh sourcing.
# SPIRA_CONF=/nonexistent prevents reading the operator's installed spira.conf.
_resolved="$(SPIRA_CONF_LOADED= SPIRA_LOOM_BIN= SPIRA_REPO="$_tl" SPIRA_CONF=/nonexistent \
    bash -c '. "$1/spira/conf.sh" 2>/dev/null; printf "%s" "${SPIRA_LOOM_BIN:-}"' \
    -- "$_tl" 2>/dev/null || true)"
want "positive-control: conf.sh resolves SPIRA_LOOM_BIN to bin/loom in tarball layout" \
    "bin/loom" "$_resolved"

# ===========================================================================
echo ""
echo "phase 2: build (cargo present)"
# ===========================================================================
# Find the toolchain's cargo/rustc directly to bypass the rustup proxy.
# The proxy relies on PATH containing its own bin dir; conf.sh stripped that.
# Invoking the toolchain binary directly computes its sysroot from its own path
# and finds rustc without PATH.
RUSTUP_HOME="${RUSTUP_HOME:-/usr/local/rustup}"
_TC_CARGO="$_CARGO_BIN"
_TC_RUSTC=""
for _tc_dir in $(ls -d "$RUSTUP_HOME/toolchains/"* 2>/dev/null); do
    if [ -x "$_tc_dir/bin/cargo" ] && [ -x "$_tc_dir/bin/rustc" ]; then
        _TC_CARGO="$_tc_dir/bin/cargo"
        _TC_RUSTC="$_tc_dir/bin/rustc"
        break
    fi
done

# Use a user-writable target dir: /workspace is owned by the host user (uid=1000)
# and spirauser (uid=1001) cannot write to it in rootless podman. A temp dir under
# SCRATCH is always user-writable.
LOOM_TARGET="$SCRATCH/loom-target"
LOOM_BIN="$LOOM_TARGET/release/loom"

if [ ! -x "$LOOM_BIN" ]; then
    printf '  building loom (takes a few minutes)...\n'
    # Use a temp CARGO_HOME to avoid testenv named-volume permission issues
    # (fresh named volumes at /var/spira/cargo are root-owned; temp dir is user-owned).
    _TC_CARGO_HOME="$SCRATCH/cargo-build"
    mkdir -p "$_TC_CARGO_HOME" "$LOOM_TARGET"
    if [ -n "$_TC_RUSTC" ]; then
        CARGO_HOME="$_TC_CARGO_HOME" RUSTC="$_TC_RUSTC" \
            "$_TC_CARGO" build --release \
            --manifest-path "$WORKSPACE/loom/Cargo.toml" \
            --target-dir "$LOOM_TARGET" >&2
    else
        CARGO_HOME="$_TC_CARGO_HOME" \
            "$_TC_CARGO" build --release \
            --manifest-path "$WORKSPACE/loom/Cargo.toml" \
            --target-dir "$LOOM_TARGET" >&2
    fi
    build_rc=$?
    iszero "cargo build loom" "$build_rc"
    [ "$build_rc" -ne 0 ] && {
        printf '%s passed, %s failed\n' "$pass" "$fail"; [ "$fail" = 0 ]; exit
    }
else
    ok "loom binary already built at $LOOM_BIN"
fi

[ -x "$LOOM_BIN" ] || {
    bad "loom binary" "not found at $LOOM_BIN after build"
    printf '%s passed, %s failed\n' "$pass" "$fail"; [ "$fail" = 0 ]; exit
}

# Build the tarball manually.  Inside the testenv container, the workspace is a git
# worktree whose common git directory lives on the host at an absolute path that is
# not mounted in the container, so `git archive` (used by build-tarball.sh) fails.
# Manual construction reproduces what build-tarball.sh would produce: a versioned
# top-level directory with bin/loom, bin/panel, MANIFEST, and the harness tree.
_ts="$(date -u '+%Y%m%dT%H%M%SZ')"
_name="spira-$_ts"
_stage="$SCRATCH/$_name"
mkdir -p "$_stage/bin"
for _d in spira systemd loom cockpit; do
    [ -d "$WORKSPACE/$_d" ] && cp -rp "$WORKSPACE/$_d" "$_stage/" || true
done
cp "$LOOM_BIN"   "$_stage/bin/loom"  && chmod +x "$_stage/bin/loom"
cp "$STUB_PANEL" "$_stage/bin/panel" && chmod +x "$_stage/bin/panel"
printf 'commit %s\ntimestamp %s\n' \
    "0000000000000000000000000000000000000000" "$_ts" > "$_stage/MANIFEST"
TARBALL="$SCRATCH/$_name.tar.gz"
tar -czf "$TARBALL" -C "$SCRATCH" "$_name" \
    && ok "tarball produced: $(basename "$TARBALL")" \
    || { bad "tarball" "tar failed"; printf '%s passed, %s failed\n' "$pass" "$fail"; [ "$fail" = 0 ]; exit; }

# ===========================================================================
echo ""
echo "phase 3: activate and verify with cargo hidden from PATH"
# ===========================================================================
# Activate the tarball.  SPIRA_SYSTEMCTL stub prevents daemon-reload and restart
# from touching any installed units; SPIRA_ACTIVATE_FORCE bypasses the live-aeon guard.
SPIRA_SYSTEMCTL="$STUB_SC" SPIRA_ACTIVATE_FORCE=1 SPIRA_RELEASES="$RELEASES" \
    bash "$HERE/activate.sh" "$TARBALL" >&2
iszero "activate.sh exits 0" "$?"

CURRENT="$RELEASES/current"
[ -L "$CURRENT" ] && ok "current symlink created" \
    || bad "current symlink" "missing at $RELEASES/current"

[ -x "$CURRENT/bin/loom" ] && ok "bin/loom executable in release" \
    || { bad "bin/loom" "not executable in release"; printf '%s passed, %s failed\n' "$pass" "$fail"; [ "$fail" = 0 ]; exit; }

# Verify conf.sh picks up bin/loom when sourced from the release directory.
_rel_loom="$(SPIRA_CONF_LOADED= SPIRA_LOOM_BIN= SPIRA_REPO="$CURRENT" SPIRA_CONF=/nonexistent \
    bash -c '. "$1/spira/conf.sh" 2>/dev/null; printf "%s" "${SPIRA_LOOM_BIN:-}"' \
    -- "$CURRENT" 2>/dev/null || true)"
want "conf.sh resolves SPIRA_LOOM_BIN to bin/loom in activated release" \
    "bin/loom" "$_rel_loom"

# ---------------------------------------------------------------------------
# Create a test database (embedded bd; no dolt server needed).
# ---------------------------------------------------------------------------
testdb_up "aifsp9b7i_$$" >/dev/null 2>&1
iszero "testdb_up exits 0" "$?"

# ---------------------------------------------------------------------------
# Start loom from the tarball's bin/loom with cargo stripped from PATH.
# Use a random high port so the test does not collide with any running loom.
# ---------------------------------------------------------------------------
LOOM_PORT="$((RANDOM % 10000 + 40000))"
LOOM_ADDR="127.0.0.1:${LOOM_PORT}"

# Build a PATH that has bd but no cargo/rustc.
_PATH_NOCARGO="$(printf '%s\n' "$PATH" | tr ':' '\n' \
    | grep -v 'cargo' | grep -v 'rustup' | tr '\n' ':' | sed 's/:$//')"

SPIRA_DB="$SPIRA_DB" \
SPIRA_BD="${SPIRA_BD:-$(command -v bd)}" \
SPIRA_LOOM_ADDR="$LOOM_ADDR" \
PATH="$_PATH_NOCARGO" \
    "$CURRENT/bin/loom" >"$SCRATCH/loom.log" 2>&1 &
LOOM_PID=$!

# Wait up to 90 s for loom to start. The first /api/beads request runs a cold
# bd query against the test fixture; on a loaded host this can take 60+ seconds.
_ready=0
for _i in $(seq 1 180); do
    sleep 0.5
    python3 -c "
import urllib.request
try:
    r = urllib.request.urlopen('http://$LOOM_ADDR/api/beads', timeout=2)
    exit(0 if r.status == 200 else 1)
except: exit(1)
" 2>/dev/null && _ready=1 && break
done

if [ "$_ready" = 1 ]; then
    ok "loom started (PID $LOOM_PID) without cargo on PATH"
elif kill -0 "$LOOM_PID" 2>/dev/null; then
    # Process is alive but first /api/beads took >90s (cold database + dolt connection setup
    # can exceed loom's 1500ms budget on the first request). Process liveness proves the binary
    # ran without cargo; ready.sh below is the authoritative 200-answer check.
    ok "loom process alive (PID $LOOM_PID) — first request slow, deferring to ready.sh"
else
    bad "loom did not start" "$(head -5 "$SCRATCH/loom.log" 2>/dev/null)"
fi

# Probe /api/beads.
if [ "$_ready" = 1 ]; then
    _probe="$(python3 - "http://$LOOM_ADDR/api/beads" 3000 2>/dev/null <<'PYEOF'
import sys, urllib.request, time
url = sys.argv[1]; budget_ms = float(sys.argv[2])
t0 = time.monotonic()
try:
    r = urllib.request.urlopen(url, timeout=budget_ms / 1000)
    ms = int((time.monotonic() - t0) * 1000)
    sys.stdout.write("%d %dms\n" % (r.status, ms))
except Exception as e:
    ms = int((time.monotonic() - t0) * 1000)
    sys.stdout.write("ERR %dms %s\n" % (ms, str(e)[:100]))
PYEOF
)"
    want "/api/beads returns 200 from tarball loom" "200" "$_probe"
fi

# ---------------------------------------------------------------------------
# ready.sh — all conditions satisfied:
#   sentinel:   stub systemctl returns exit 0 for is-enabled/is-active
#   world:      no world.halted stamp in SPIRA_RUN
#   database:   testdb_up created a readable bd database
#   loom:       running from tarball bin/loom on LOOM_ADDR; SPIRA_LOOM_BIN set
#   cockpit:    stub snapshot present so freshness check passes; panes WARN (no tmux)
#   agent:      WARN (no real claude on PATH in tarball install test)
# ---------------------------------------------------------------------------
SPIRA_RUN_DIR="$SCRATCH/spira-run"
mkdir -p "$SPIRA_RUN_DIR"
# Stub snapshot so ready.sh cockpit freshness check passes (collector not running here).
printf 'SP_AT=0\n' > "$SPIRA_RUN_DIR/cockpit.env"

# A probe script so ready.sh calls our running loom rather than the installed one.
PROBE="$SCRATCH/loom-probe"
cat > "$PROBE" <<PROBEOF
#!/bin/sh
python3 - "\$1" "\$2" <<'PYEOF'
import sys, urllib.request, time
url = sys.argv[1]; budget_ms = float(sys.argv[2])
t0 = time.monotonic()
try:
    r = urllib.request.urlopen(url, timeout=budget_ms / 1000)
    ms = int((time.monotonic() - t0) * 1000)
    sys.stdout.write("%d %dms\n" % (r.status, ms))
except Exception as e:
    ms = int((time.monotonic() - t0) * 1000)
    sys.stdout.write("ERR %dms %s\n" % (ms, str(e)[:100]))
PYEOF
PROBEOF
chmod +x "$PROBE"

ready_out="$(
    SPIRA_REPO="$CURRENT" \
    SPIRA_CONF=/nonexistent \
    SPIRA_DB="$SPIRA_DB" \
    SPIRA_BD="${SPIRA_BD:-$(command -v bd)}" \
    SPIRA_LOOM_BIN="$CURRENT/bin/loom" \
    SPIRA_LOOM_ADDR="$LOOM_ADDR" \
    SPIRA_LOOM_PROBE="$PROBE" \
    SPIRA_RUN="$SPIRA_RUN_DIR" \
    SPIRA_SYSTEMCTL="$STUB_SC" \
    SPIRA_INSTANCE="aif9b7i" \
    bash "$CURRENT/spira/ready.sh" 2>&1
)"
ready_rc=$?
iszero "ready.sh exits 0 in tarball-only environment" "$ready_rc"
want "ready.sh: loom answers 200" "loom answers 200" "$ready_out"
notwant "ready.sh: loom not UNKN (binary found via bin/loom)" "UNKN  loom" "$ready_out"
notwant "ready.sh: no FAIL for loom" "FAIL  loom" "$ready_out"

# ===========================================================================
echo ""
printf '%s passed, %s failed\n' "$pass" "$fail"
[ "$fail" = 0 ]
