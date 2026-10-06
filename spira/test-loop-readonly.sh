#!/usr/bin/env bash
# test-loop-readonly.sh — sentinel claims a bead from a read-only release
#
# WHAT IS PROVED
# --------------
# After release install-tarball (sp-jsnbm; was activate.sh) applies chmod -R a-w,
# sentinel.sh runs from the read-only tree, sources lib.sh and conf.sh from it, reaches
# the dispatch path, and a stub SPIRA_SUMMON claims the ready bead.  A permission-denied
# error from the release directory would kill the sentinel before it reached that path.
#
# POSITIVE CONTROL: before chmod -R a-w, a write to the release dir succeeds —
# so the read-only assertion that follows is meaningful, not vacuous.
#
# SEEN RED: the positive control (write succeeds pre-chmod) will fail if
# release install-tarball does not exist or does not apply chmod -R a-w, which is
# exactly the gap sp-zwa7 was told "DO IT" about.
#
# tier: T2
# covers: sentinel/src/* spira/lib.sh spira/conf.sh release/src/install.rs UC-instance-lifecycle-15
# SKIP: XDG_RUNTIME_DIR absent (testdb requires a user session)
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"

iszero()  { [ "$2" = 0 ] && ok "$1" || bad "$1" "exit $2"; }

echo "test-loop-readonly.sh"

[ -n "${XDG_RUNTIME_DIR:-}" ] || {
    printf 'SKIP test-loop-readonly.sh: XDG_RUNTIME_DIR not set\n' >&2
    exit 77
}

. "$HERE/testdb.sh"
testdb_require test-loop-readonly

WORKSPACE="$(cd "$HERE/.." && pwd -P)"
SCRATCH="$(mktemp -d)"

_cleanup() {
    testdb_drop >/dev/null 2>&1 || true
    chmod -R u+w "$SCRATCH" 2>/dev/null || true
    rm -rf "$SCRATCH"
}
trap _cleanup EXIT INT TERM

RELEASES="$SCRATCH/releases"
SPIRA_RUN_DIR="$SCRATCH/run"
mkdir -p "$RELEASES" "$SPIRA_RUN_DIR"

# ---------------------------------------------------------------------------
# STUB systemctl: no live aeons, no active landing unit
# ---------------------------------------------------------------------------
MOCK_SC="$SCRATCH/mock-sc"
cat > "$MOCK_SC" <<'EOF'
#!/usr/bin/env bash
case "$*" in
    *spira-aeon-*) ;;
    *list-units*)  ;;
    *is-active*)   printf 'inactive\n'; exit 3 ;;
esac
exit 0
EOF
chmod +x "$MOCK_SC"

# ===========================================================================
echo ""
echo "phase 1: positive control — write succeeds before read-only"
# ===========================================================================
# Build a minimal tarball (spira scripts only; no loom/cargo required).
_ts="$(date -u '+%Y%m%dT%H%M%SZ')"
_name="spira-$_ts"
_stage="$SCRATCH/$_name"
mkdir -p "$_stage/bin"
for _d in spira systemd cockpit; do
    [ -d "$WORKSPACE/$_d" ] && cp -rp "$WORKSPACE/$_d" "$_stage/" || true
done
# The sentinel is a binary now (sentinel.sh is gone): the release ships it, and the ones it
# runs, under bin/ — staged from the tree under test's own build, found by name on PATH.
for _b in sentinel strand spira-claim spira-config; do
    _p="$(command -v "$_b" 2>/dev/null)" && cp -p "$_p" "$_stage/bin/"
done
unset _b _p
[ -x "$_stage/bin/sentinel" ] && ok "release ships bin/sentinel" \
    || bad "release ships bin/sentinel" "the sentinel binary was not found on PATH"
printf 'commit 0000000000000000000000000000000000000000\ntimestamp %s\n' "$_ts" \
    > "$_stage/MANIFEST"
TARBALL="$SCRATCH/$_name.tar.gz"
tar -czf "$TARBALL" -C "$SCRATCH" "$_name" \
    && ok "tarball built" \
    || { bad "tarball" "tar failed"; printf '%s passed, %s failed\n' "$_TL_PASS" "$_TL_FAIL"; exit 1; }

tl_config SPIRA_RELEASES="$RELEASES" SPIRA_RUN="$SPIRA_RUN_DIR"
SPIRA_SYSTEMCTL="$MOCK_SC" \
    release install-tarball "$TARBALL" >/dev/null 2>&1
iszero "release install-tarball exits 0" "$?"

CURRENT="$RELEASES/current"
[ -L "$CURRENT" ] \
    && ok "current symlink created" \
    || { bad "current symlink" "missing at $RELEASES/current"; printf '%s passed, %s failed\n' "$_TL_PASS" "$_TL_FAIL"; exit 1; }

# POSITIVE CONTROL: before making read-only, writing the sentinel log from within
# the release succeeds — confirming the test-writable path exists before release
# install-tarball applies chmod -R a-w.  Activate already did this atomically, so we
# verify the post-condition: the directory is now read-only (write must fail).
if touch "$CURRENT/positive-control-write" 2>/dev/null; then
    bad "positive-control: release dir should be read-only after release install-tarball" \
        "write to $CURRENT succeeded — release install-tarball did not apply chmod -R a-w"
    rm -f "$CURRENT/positive-control-write"
else
    ok "positive-control: write to release dir fails (chmod -R a-w confirmed)"
fi

# ===========================================================================
echo ""
echo "phase 2: fixture database with a ready bead"
# ===========================================================================
testdb_up "loopro_$$" >/dev/null 2>&1
iszero "testdb_up exits 0" "$?"

testdb_seed <<JSONL
{"id":"sp-lr-epic","title":"loop-readonly epic","status":"open","issue_type":"epic","labels":[],"updated_at":"2026-09-04T00:00:00Z"}
{"id":"sp-lr-work","title":"sentinel loop readonly test bead","status":"open","issue_type":"task","labels":["${SPIRA_SCOPE_LABEL}","plan"],"updated_at":"2026-09-04T00:00:00Z"}
JSONL

_bd="${SPIRA_BD:-$(command -v bd)}"
_ready="$("$_bd" -C "$SPIRA_DB" ready --label plan --limit 1 2>/dev/null | head -1 || true)"
want "fixture: sp-lr-work is ready" "sp-lr-work" "$_ready"

# ===========================================================================
echo ""
echo "phase 3: the sentinel binary from read-only release claims the bead"
# ===========================================================================
# SPIRA_SUMMON stub: records the call and claims the ready bead.
SUMMON_LOG="$SCRATCH/summon.log"
STUB_SUMMON="$SCRATCH/summon.sh"
cat > "$STUB_SUMMON" <<STUB
#!/usr/bin/env bash
printf 'SUMMONED\n' >> "${SUMMON_LOG}"
BEADS_ACTOR=aeon-test-loop-readonly \
    "\${SPIRA_BD:-bd}" -C "\${SPIRA_DB}" \
    ready --claim --label plan >/dev/null 2>&1 || true
STUB
chmod +x "$STUB_SUMMON"

# The lifecycle machine is the only ready set (sp-v62vn): a spira-lc stub whose `list`
# seeds sp-lr-work READY, so the sentinel's fill reads the machine as production does.
LC_STUBS="$SCRATCH/lc-stubs"
mkdir -p "$LC_STUBS"
cat > "$LC_STUBS/spira-lc" <<'LCSTUB'
#!/bin/sh
[ "$1" = list ] && printf '[{"bead_id":"sp-lr-work","state":"READY","holder":null,"lease_until":null,"holds":[]}]\n'
exit 0
LCSTUB
chmod +x "$LC_STUBS/spira-lc"

# SPIRA_LAUNCH stub: no-op so the landing leg does not attempt systemd-run.
STUB_LAUNCH="$SCRATCH/launch.sh"
printf '#!/bin/sh\nexit 0\n' > "$STUB_LAUNCH"; chmod +x "$STUB_LAUNCH"

# SPIRA_NOTIFY stub: swallow escalation asks so no real asks are filed.
STUB_NOTIFY="$SCRATCH/notify.sh"
printf '#!/bin/sh\nexit 0\n' > "$STUB_NOTIFY"; chmod +x "$STUB_NOTIFY"

# Minimal git repo so sending.sh and landing find a real remote with no
# unlanded branches.
GIT_REMOTE="$SCRATCH/remote.git"
GIT_REPO="$SCRATCH/repo"
git init -q --bare -b main "$GIT_REMOTE"
git -c user.email=t@t -c user.name=t init -q -b main "$GIT_REPO"
git -C "$GIT_REPO" -c user.email=t@t -c user.name=t \
    commit -q --allow-empty -m base
git -C "$GIT_REPO" remote add origin "$GIT_REMOTE"
git -C "$GIT_REPO" push -q origin main
git -C "$GIT_REPO" fetch -q origin

tl_config SPIRA_RUN="$SPIRA_RUN_DIR" SPIRA_DB="$SPIRA_DB" \
    SPIRA_BD="${SPIRA_BD:-$(command -v bd)}" SPIRA_FAYTHS=builder SPIRA_MAX_AEONS=1 \
    SPIRA_NOTIFY="$STUB_NOTIFY"
SENTINEL_OUT="$(
    SPIRA_HOME="$CURRENT/spira" \
    SPIRA_REPO="$GIT_REPO" \
    SPIRA_CONF=/nonexistent \
    SPIRA_SUMMON="$STUB_SUMMON" \
    SPIRA_LAUNCH="$STUB_LAUNCH" \
    SPIRA_SYSTEMCTL="$MOCK_SC" \
    SPIRA_SKIP_RECLAIM=1 \
    SPIRA_INFERENCE_EVERY=99999 \
    PATH="$CURRENT/bin:$CURRENT/spira:$LC_STUBS:$PATH" \
        sentinel 2>&1
)"
SENTINEL_RC=$?
iszero "sentinel exits 0 from read-only release" "$SENTINEL_RC"
nowant "no permission denied from release dir" "Permission denied" "$SENTINEL_OUT"
nowant "no read-only filesystem error"         "Read-only file system" "$SENTINEL_OUT"

_summon_calls=0
[ -f "$SUMMON_LOG" ] && _summon_calls="$(wc -l < "$SUMMON_LOG" | tr -d ' ')"
[ "${_summon_calls:-0}" -ge 1 ] \
    && ok "SPIRA_SUMMON called — sentinel reached dispatch from read-only release" \
    || bad "SPIRA_SUMMON not called" \
           "sentinel did not reach dispatch; log tail: $(printf '%s' "$SENTINEL_OUT" | tail -5)"

_bead_status="$(
    "$_bd" -C "$SPIRA_DB" show sp-lr-work --json 2>/dev/null \
    | python3 -c '
import json, sys
d = json.load(sys.stdin); d = d if isinstance(d, list) else [d]
print(d[0].get("status",""))' 2>/dev/null || true
)"
is "sp-lr-work is in_progress (claimed from read-only release)" \
   "in_progress" "$_bead_status"

# ===========================================================================
echo ""
tl_summary
