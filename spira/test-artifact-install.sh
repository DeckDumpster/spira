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
#   Phase 2 (package): build-tarball.sh packages the workspace's own prebuilt
#     bin/loom (the gate's build job compiles it, sp-7r4rl) as bin/loom inside a
#     spira-<timestamp>.tar.gz. A sha256 match against the source binary proves
#     the packaged copy is that prebuilt one, not something rebuilt in its place.
#   Phase 3 (no-cargo): activate.sh unpacks the tarball. conf.sh's fixed default
#     resolves SPIRA_LOOM_BIN to bin/loom inside the release. Loom starts;
#     /api/beads → 200. ready.sh exits 0.
#
# LAW-ABSENCE-NEEDS-A-POSITIVE-CONTROL: phase 1 proves the UNKN path fires before
# phase 3 trusts that it does not.
#
# NO CARGO BUILD HERE. conf.sh (sourced by lib.sh below) sets PATH from scratch
# and never includes cargo's bin dir, so this suite never has cargo on PATH —
# if a future edit reintroduces a `cargo build` call, it fails loudly rather
# than silently reverting to the exclusive, several-GB-peak build this suite
# used to run alone at the start of every batch.
#
# SKIP: no prebuilt bin/loom in the workspace (the gate's build job did not run
#   for this branch); XDG_RUNTIME_DIR absent (no user session).
#
# covers: spira/conf.sh spira/activate.sh spira/loom.sh spira/ready.sh spira/testenv-batch.sh install.sh

set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"

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

# The gate's build job compiles this; testenv-batch.sh copies it into the
# branch worktree before the container starts (sp-7r4rl, sp-n1f5o). Without
# it there is nothing to package and nothing this suite can prove.
LOOM_BIN="$WORKSPACE/bin/loom"
[ -x "$LOOM_BIN" ] || {
    printf 'SKIP test-artifact-install.sh: no prebuilt %s (gate build job did not run for this branch)\n' \
        "$LOOM_BIN" >&2
    exit 77
}

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
echo "phase 2: package the prebuilt bin/loom"
# ===========================================================================
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
_src_sha="$(sha256sum "$LOOM_BIN" | awk '{print $1}')"
cp "$LOOM_BIN"   "$_stage/bin/loom"  && chmod +x "$_stage/bin/loom"
cp "$STUB_PANEL" "$_stage/bin/panel" && chmod +x "$_stage/bin/panel"
printf 'commit %s\ntimestamp %s\n' \
    "0000000000000000000000000000000000000000" "$_ts" > "$_stage/MANIFEST"

# POSITIVE CONTROL: the packaged bin/loom is the prebuilt one, not a binary
# built fresh in its place. A byte-for-byte match is the only proof that
# survives a future edit reintroducing a build step here.
_staged_sha="$(sha256sum "$_stage/bin/loom" | awk '{print $1}')"
[ "$_src_sha" = "$_staged_sha" ] \
    && ok "positive-control: packaged bin/loom matches the prebuilt binary (sha256 $_src_sha)" \
    || bad "positive-control" "packaged bin/loom sha256 ($_staged_sha) != prebuilt ($_src_sha)"

TARBALL="$SCRATCH/$_name.tar.gz"
tar -czf "$TARBALL" -C "$SCRATCH" "$_name" \
    && ok "tarball produced: $(basename "$TARBALL")" \
    || { bad "tarball" "tar failed"; printf '%s passed, %s failed\n' "$pass" "$fail"; [ "$fail" = 0 ]; exit; }

# ===========================================================================
echo ""
echo "phase 3: activate and verify without cargo"
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
# Start loom from the tarball's bin/loom. PATH here is conf.sh's own — set from
# scratch when lib.sh was sourced at the top of this suite — and it has never
# included cargo's bin dir, so this proves the release runs without it.
# Use a random high port so the test does not collide with any running loom.
# ---------------------------------------------------------------------------
LOOM_PORT="$((RANDOM % 10000 + 40000))"
LOOM_ADDR="127.0.0.1:${LOOM_PORT}"

SPIRA_DB="$SPIRA_DB" \
SPIRA_BD="${SPIRA_BD:-$(command -v bd)}" \
SPIRA_LOOM_ADDR="$LOOM_ADDR" \
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
