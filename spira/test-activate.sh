#!/usr/bin/env bash
# test-activate.sh — activate.sh: unpack beside, swap current, daemon-reload, restart
#
# PROPERTIES UNDER TEST
# ---------------------
# 1. Basic activation: tarball unpacks to a versioned dir, current symlink points to it.
# 2. Previous release is still present after activation.
# 3. The activated release directory is read-only.
# 4. Prune removes releases beyond the keep limit.
# 5. Prune never removes the current release (even if it is beyond the limit) — a T1
#    case on the extracted _prune_candidates (lib.sh), because activate.sh always swings
#    current to the just-unpacked release before pruning runs, so the integration path
#    above can never present a current target outside the keep window (gap G4).
# 6. Refuses while aeons are live for this instance.
# 7. SPIRA_ACTIVATE_FORCE=1 overrides the live-aeon guard.
# 8. daemon-reload precedes the restart of changed units (gap G5), and restart excludes
#    aeon units.
# 9. --dry-run reports intended actions without touching the release directory.
# 10. Rollback is re-activation of an already-unpacked release: unpack is skipped and
#     current swings back (merged from test-rollback.sh; UC-instance-lifecycle-11).
#
# FAIL-FIRST: each property is verified against the UNFIXED tree (no activate.sh)
# before any fix is applied, confirming the suite catches the absence.
#
# tier: T2
# covers: spira/activate.sh spira/conf.sh spira/lib.sh UC-instance-lifecycle-08 UC-instance-lifecycle-09 UC-instance-lifecycle-10 UC-instance-lifecycle-11
# host-reason: exercises tmpfs atomic rename and a mock systemctl; fully self-contained
#              in temp dirs — no real systemd or database required
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"
is0()     { [ "$2" = 0 ] && ok "$1" || bad "$1" "exit $2"; }
not0()    { [ "$2" != 0 ] && ok "$1" || bad "$1" "wanted non-zero, got 0"; }
islink()  {
    if [ -L "$2" ] && [ "$(readlink "$2")" = "$3" ]; then
        ok "$1"
    else
        bad "$1" "wanted symlink $2 -> $3 (got: $(readlink "$2" 2>/dev/null || echo '(missing)'))"
    fi
}

echo "test-activate.sh"

ACTIVATE="$HERE/activate.sh"

# ---------------------------------------------------------------------------
# FAIL-FIRST: confirm activate.sh is present before running any property test.
# Without it every invocation below will fail. This check makes the failure
# legible instead of producing confusing downstream errors.
# ---------------------------------------------------------------------------
if [ ! -x "$ACTIVATE" ]; then
    bad "fail-first: activate.sh must exist and be executable at $ACTIVATE" \
        "file missing or not executable"
    tl_summary
    exit
fi

TMP="$(mktemp -d)"; trap 'chmod -R u+w "$TMP" 2>/dev/null; rm -rf "$TMP"' EXIT

RELEASES="$TMP/releases"
SPIRA_RUN_DIR="$TMP/run"
TARBALLS="$TMP/tarballs"
SC_LOG="$TMP/sc.log"
MOCK_SC="$TMP/mock-sc"

mkdir -p "$RELEASES" "$SPIRA_RUN_DIR" "$TARBALLS"

# ---------------------------------------------------------------------------
# Mock systemctl — records every call; returns live aeons when MOCK_AEONS is set.
# Handles the two list-units patterns activate.sh uses:
#   (a) spira-aeon-*-<instance>.service  — live-aeon guard
#   (b) spira-*.service                  — units to restart
# ---------------------------------------------------------------------------
cat > "$MOCK_SC" <<EOF
#!/usr/bin/env bash
printf 'SC: %s\n' "\$*" >> "${SC_LOG}"
case "\$*" in
    *spira-aeon-*)
        printf '%s\n' "\${MOCK_AEONS:-}"
        ;;
    *list-units*)
        printf 'spira-sentinel-prod.service loaded active running Sentinel\n'
        ;;
esac
exit "\${MOCK_SC_EXIT:-0}"
EOF
chmod +x "$MOCK_SC"

# ---------------------------------------------------------------------------
# make_tarball <release-name> — produces $TARBALLS/<release-name>.tar.gz
# The tarball unpacks to a top-level directory named <release-name>, matching
# the format build-tarball.sh produces.
# ---------------------------------------------------------------------------
make_tarball() {
    local name="$1"
    local stage; stage="$(mktemp -d)"
    mkdir -p "$stage/$name/spira"
    printf '# stub sentinel\n' > "$stage/$name/spira/sentinel.sh"
    printf 'commit deadbeefdeadbeefdeadbeefdeadbeefdeadbeef\ntimestamp 2026-09-15T00:00:00Z\n' \
        > "$stage/$name/MANIFEST"
    tar -czf "$TARBALLS/$name.tar.gz" -C "$stage" "$name"
    rm -rf "$stage"
    printf '%s/%s.tar.gz\n' "$TARBALLS" "$name"
}

# ---------------------------------------------------------------------------
# run_activate [<extra env>...] -- <activate args...>
# Runs activate.sh in a minimal isolated environment.
#   SPIRA_DB=/nonexistent    — no database, skips bd schema check in conf.sh
#   SPIRA_CONF=/nonexistent  — no config file, all keys use defaults or env
#   SPIRA_HOME=$HERE         — the real spira/ dir so lib.sh can be sourced
# ---------------------------------------------------------------------------
run_activate() {
    local extra_env=() activate_args=() in_args=0
    for _a in "$@"; do
        [ "$_a" = "--" ] && { in_args=1; continue; }
        [ "$in_args" = 1 ] && { activate_args+=("$_a"); continue; }
        extra_env+=("$_a")
    done
    unset _a in_args

    > "$SC_LOG"
    env -i \
        "PATH=$PATH" \
        "HOME=$HOME" \
        "SPIRA_HOME=$HERE" \
        "SPIRA_DB=/nonexistent-spira-db" \
        "SPIRA_RUN=$SPIRA_RUN_DIR" \
        "SPIRA_CONF=/nonexistent" \
        "SPIRA_RELEASES=$RELEASES" \
        "SPIRA_INSTANCE=prod" \
        "SPIRA_SYSTEMCTL=$MOCK_SC" \
        "SC_LOG=$SC_LOG" \
        "${extra_env[@]+"${extra_env[@]}"}" \
        bash "$ACTIVATE" "${activate_args[@]+"${activate_args[@]}"}" 2>&1
}

# ==========================================================================
echo
echo "PROPERTY 1+2+3: basic activation, previous release present, read-only"
# ==========================================================================

# Plant an existing release and set current to it, so we can verify it survives.
OLD_NAME="spira-20260914T000000Z"
mkdir -p "$RELEASES/$OLD_NAME/spira"
ln -s "$OLD_NAME" "$RELEASES/current"

TB="$(make_tarball "spira-20260915T120000Z")"
NEW_NAME="spira-20260915T120000Z"

_out="$(run_activate -- "$TB")"
_rc=$?
is0  "basic: exits 0"              "$_rc"
islink "basic: current -> new"     "$RELEASES/current" "$NEW_NAME"

if [ -d "$RELEASES/$NEW_NAME" ]; then
    ok "basic: new release dir present"
else
    bad "basic: new release dir present" "directory missing: $RELEASES/$NEW_NAME"
fi

if [ -d "$RELEASES/$OLD_NAME" ]; then
    ok "basic: old release still present"
else
    bad "basic: old release still present" "previous release was removed: $OLD_NAME"
fi

# Read-only: the release dir should refuse a write.
if touch "$RELEASES/$NEW_NAME/test-write" 2>/dev/null; then
    bad "basic: release dir is read-only" "write succeeded — dir is not read-only"
    rm -f "$RELEASES/$NEW_NAME/test-write"
else
    ok "basic: release dir is read-only"
fi

# ==========================================================================
echo
echo "PROPERTY 4+5: prune removes excess releases, never current"
# ==========================================================================
# Reset the releases dir for a clean prune test.
chmod -R u+w "$RELEASES" 2>/dev/null; rm -rf "$RELEASES"
mkdir -p "$RELEASES"

# Create KEEP=3 "old" releases.
for i in 1 2 3 4 5; do
    local_name="spira-2026090${i}T000000Z"
    mkdir -p "$RELEASES/$local_name/spira"
done
# Set current to the oldest so it would be beyond the limit if deleted.
ln -s "spira-20260901T000000Z" "$RELEASES/current"

TB="$(make_tarball "spira-20260915T130000Z")"
NEW_NAME="spira-20260915T130000Z"

_out="$(run_activate "SPIRA_RELEASES_KEEP=3" -- "$TB")"
_rc=$?
is0 "prune: activation exits 0" "$_rc"
islink "prune: current -> new" "$RELEASES/current" "$NEW_NAME"

# After keep=3, with 5 old releases + 1 new, we should keep 3 newest:
# new (1) + 4th oldest + 5th oldest = the three newest dirs.
# The oldest two (20260901 and 20260902) should be gone.
if [ ! -d "$RELEASES/spira-20260901T000000Z" ] && \
   [ ! -d "$RELEASES/spira-20260902T000000Z" ]; then
    ok "prune: oldest releases removed"
else
    bad "prune: oldest releases removed" \
        "old dirs still present after prune (keep=3)"
fi

# current target must exist — activation always repoints current to the release it
# just unpacked, so this never exercises "current beyond the keep window" (gap G4);
# that case is a T1 call on _prune_candidates directly, below.
_cur="$RELEASES/$(readlink "$RELEASES/current" 2>/dev/null)"
if [ -d "$_cur" ]; then
    ok "prune: current target still present"
else
    bad "prune: current target still present" "current -> $(readlink "$RELEASES/current") does not exist"
fi

# ==========================================================================
echo
echo "PROPERTY 5 (T1, gap G4): _prune_candidates never names the current release"
# activate.sh swings current to the just-unpacked release BEFORE pruning, so the
# integration path above can never present a current target outside the keep
# window — the "protect current" branch in _prune_candidates is dead code there.
# This calls the extracted function (lib.sh) directly with a current target held
# apart from the newest releases, which the integration path cannot construct.
# ==========================================================================
PRUNE_DIR="$TMP/prune-candidates"; mkdir -p "$PRUNE_DIR"
for i in 1 2 3 4 5; do
    mkdir -p "$PRUNE_DIR/spira-2026090${i}T000000Z"
done

run_prune_candidates() {
    env -i "PATH=$PATH" "HOME=$HOME" "SPIRA_HOME=$HERE" \
        "SPIRA_DB=/nonexistent-spira-db" "SPIRA_RUN=$SPIRA_RUN_DIR" \
        "SPIRA_CONF=/nonexistent" "SPIRA_INSTANCE=prod" \
        bash -c '. "$SPIRA_HOME/lib.sh" && _prune_candidates "$@"' _ "$@"
}

# POSITIVE CONTROL: with no current target to protect, keep=1 proposes every
# release older than the newest — including spira-20260901 (the one the property
# below must exclude), proving the matcher can find an offender at all.
_out="$(run_prune_candidates "$PRUNE_DIR" 1 "")"
want "prune-candidates: positive control names the oldest release" \
    "spira-20260901T000000Z" "$_out"

# THE PROPERTY: current pinned to the oldest release (beyond keep=1) must never
# appear among the candidates, while other excess releases still do.
_out="$(run_prune_candidates "$PRUNE_DIR" 1 "spira-20260901T000000Z")"
nowant "prune-candidates: never names the current release" \
    "spira-20260901T000000Z" "$_out"
want "prune-candidates: still names other excess releases" \
    "spira-20260902T000000Z" "$_out"

# ==========================================================================
echo
echo "PROPERTY 6: refuses while aeons are live"
# ==========================================================================
chmod -R u+w "$RELEASES" 2>/dev/null; rm -rf "$RELEASES"; mkdir -p "$RELEASES"
TB="$(make_tarball "spira-20260915T140000Z")"

_out="$(run_activate "MOCK_AEONS=spira-aeon-abc-prod.service" -- "$TB")"
_rc=$?
not0  "aeon-guard: non-zero exit with live aeons" "$_rc"
want  "aeon-guard: names 'aeon'" "aeon" "$_out"
want  "aeon-guard: names the override" "SPIRA_ACTIVATE_FORCE" "$_out"
# current must NOT have been created — activation was refused.
if [ ! -e "$RELEASES/current" ]; then
    ok "aeon-guard: current was not created"
else
    bad "aeon-guard: current was not created" "current exists despite refusal"
fi

# ==========================================================================
echo
echo "PROPERTY 7: SPIRA_ACTIVATE_FORCE=1 overrides the live-aeon guard"
# ==========================================================================
chmod -R u+w "$RELEASES" 2>/dev/null; rm -rf "$RELEASES"; mkdir -p "$RELEASES"
TB="$(make_tarball "spira-20260915T150000Z")"
NEW_NAME="spira-20260915T150000Z"

_out="$(run_activate "MOCK_AEONS=spira-aeon-abc-prod.service" \
                     "SPIRA_ACTIVATE_FORCE=1" -- "$TB")"
_rc=$?
is0    "force: exits 0 with SPIRA_ACTIVATE_FORCE=1" "$_rc"
islink "force: current -> new" "$RELEASES/current" "$NEW_NAME"

# ==========================================================================
echo
echo "PROPERTY 8: daemon-reload and restart are called after symlink swap"
# ==========================================================================
chmod -R u+w "$RELEASES" 2>/dev/null; rm -rf "$RELEASES"; mkdir -p "$RELEASES"
TB="$(make_tarball "spira-20260915T160000Z")"

run_activate -- "$TB" >/dev/null 2>&1
# daemon-reload must appear in the systemctl log AFTER any list-units call.
if grep -q 'daemon-reload' "$SC_LOG"; then
    ok "restart: daemon-reload called"
else
    bad "restart: daemon-reload called" "not found in SC_LOG"
fi
if grep -q 'restart' "$SC_LOG"; then
    ok "restart: restart called"
else
    bad "restart: restart called" "not found in SC_LOG"
fi
# Aeon units must not be in the restart call.
if grep 'restart' "$SC_LOG" | grep -q 'spira-aeon-'; then
    bad "restart: no aeon in restart" "aeon unit found in restart call"
else
    ok "restart: no aeon unit in restart"
fi

# Gap G5: daemon-reload is claimed to precede the restart of changed units, but that
# order was never asserted — only that both calls happened somewhere in the log.
_reload_at="$(grep -n 'daemon-reload' "$SC_LOG" | head -1 | cut -d: -f1)"
_restart_at="$(grep -n -- '--user restart' "$SC_LOG" | head -1 | cut -d: -f1)"
if [ -n "$_reload_at" ] && [ -n "$_restart_at" ] && [ "$_reload_at" -lt "$_restart_at" ]; then
    ok "restart: daemon-reload precedes restart (gap G5)"
else
    bad "restart: daemon-reload precedes restart (gap G5)" \
        "reload at line ${_reload_at:-none}, restart at line ${_restart_at:-none} in SC_LOG"
fi

# ==========================================================================
echo
echo "PROPERTY 9: --dry-run does not unpack or swap"
# ==========================================================================
chmod -R u+w "$RELEASES" 2>/dev/null; rm -rf "$RELEASES"; mkdir -p "$RELEASES"
TB="$(make_tarball "spira-20260915T170000Z")"
NEW_NAME="spira-20260915T170000Z"

_out="$(run_activate -- --dry-run "$TB")"
_rc=$?
is0     "dry-run: exits 0" "$_rc"
want    "dry-run: says DRY RUN" "DRY RUN" "$_out"
if [ ! -e "$RELEASES/current" ]; then
    ok "dry-run: current was not created"
else
    bad "dry-run: current was not created" "current exists after --dry-run"
fi
if [ ! -d "$RELEASES/$NEW_NAME" ]; then
    ok "dry-run: release dir was not created"
else
    bad "dry-run: release dir was not created" "release dir exists after --dry-run"
fi

# ==========================================================================
echo
echo "PROPERTY 10: rollback is re-activation of an already-unpacked release"
# Rolling back IS activating the previous release: the same activate.sh code path in
# both directions, with no separate rollback path (merged from test-rollback.sh — the
# unique leg was this re-activation step; the rest duplicated properties 1-3 above).
# SEEN RED against the unfixed tree: activate.sh exited 1 with "release already present".
# ==========================================================================
chmod -R u+w "$RELEASES" 2>/dev/null; rm -rf "$RELEASES"; mkdir -p "$RELEASES"
TB_A="$(make_tarball "spira-20260915T180000Z")"
REL_A="spira-20260915T180000Z"
TB_B="$(make_tarball "spira-20260915T190000Z")"
REL_B="spira-20260915T190000Z"

run_activate -- "$TB_A" >/dev/null 2>&1
run_activate -- "$TB_B" >/dev/null 2>&1
islink "rollback: current -> B before rollback" "$RELEASES/current" "$REL_B"

_out="$(run_activate -- "$TB_A")"
_rc=$?
is0    "rollback: re-activating A exits 0"            "$_rc"
want   "rollback: skips unpack (dir already present)" "already present" "$_out"
islink "rollback: current -> A again"                 "$RELEASES/current" "$REL_A"
if grep -q 'restart' "$SC_LOG"; then
    ok "rollback: units restarted after re-activation"
else
    bad "rollback: units restarted after re-activation" "not found in SC_LOG"
fi

# ==========================================================================
echo
tl_summary
