#!/usr/bin/env bash
# test-rollback.sh — rollback is activation of the previous release, happy path
#
# PROPERTY UNDER TEST
# -------------------
# Rolling back IS activating the previous release: the same activate.sh code
# path in both directions, with no separate rollback path.
#
#   activate A          ->  units execute A's code
#   activate B over A   ->  units execute B's code   [positive control]
#   activate A (rollback via same activate.sh)  ->  units execute A's code
#
# "Execute A's code" is observed from the process, not from the symlink:
# probe.sh in each release resolves its own real path via pwd -P, so the
# assertion reads what the restarted unit actually ran FROM.
#
# POSITIVE CONTROL
# ----------------
# The mid-state assertion (units executing B) genuinely fails if activation of
# B did not take: probe.sh follows the current symlink, so if current still
# points at A, the state file records A's path and the B-assertion fails.
#
# FAIL-FIRST
# ----------
# The rollback step (activate A when A's dir already exists) is the property
# this test adds. Against the unfixed tree it fails: activate.sh exits 1 with
# "release already present". The test is SEEN RED there; after the fix it is
# SEEN GREEN. Both states are exercised in this session.
#
# covers: spira/activate.sh
# host-reason: exercises tmpfs atomic rename and a mock systemctl; fully
#              self-contained in temp dirs — no real systemd or database required
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
pass=0; fail=0
ok()   { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()  { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()   { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
is0()  { [ "$2" = 0 ] && ok "$1" || bad "$1" "exit $2"; }
want() { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }

echo "test-rollback.sh"

ACTIVATE="$HERE/activate.sh"

if [ ! -x "$ACTIVATE" ]; then
    bad "fail-first: activate.sh must exist and be executable" \
        "file missing or not executable"
    printf '\n%d passed, %d failed\n' "$pass" "$fail"
    exit 1
fi

TMP="$(mktemp -d)"; trap 'chmod -R u+w "$TMP" 2>/dev/null; rm -rf "$TMP"' EXIT

RELEASES="$TMP/releases"
SPIRA_RUN_DIR="$TMP/run"
TARBALLS="$TMP/tarballs"
SC_LOG="$TMP/sc.log"
MOCK_SC="$TMP/mock-sc"
UNIT_STATE_FILE="$TMP/unit-state"

mkdir -p "$RELEASES" "$SPIRA_RUN_DIR" "$TARBALLS"

# make_tarball <release-name>
# Produces a tarball with probe.sh. When the mock systemctl calls probe.sh
# after restart, probe.sh resolves its own real directory via pwd -P and writes
# that path to UNIT_STATE_FILE. A path through the symlink becomes the actual
# release dir, so the state file reflects which release the unit is running FROM.
make_tarball() {
    local name="$1"
    local stage; stage="$(mktemp -d)"
    mkdir -p "$stage/$name/spira"
    cat > "$stage/$name/spira/probe.sh" <<'PROBE'
#!/usr/bin/env bash
resolved="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
printf '%s\n' "$resolved" > "${UNIT_STATE_FILE}"
PROBE
    chmod +x "$stage/$name/spira/probe.sh"
    tar -czf "$TARBALLS/$name.tar.gz" -C "$stage" "$name"
    rm -rf "$stage"
    printf '%s/%s.tar.gz\n' "$TARBALLS" "$name"
}

# Mock systemctl:
#   list-units *spira-aeon-* -> nothing (no live aeons)
#   list-units *             -> one active service unit
#   restart                  -> run probe.sh from the current release; this is
#                               what "the unit executes" — observed from the
#                               process via pwd -P, not by reading the symlink
cat > "$MOCK_SC" <<'EOF'
#!/usr/bin/env bash
printf 'SC: %s\n' "$*" >> "${SC_LOG}"
case "$*" in
    *spira-aeon-*) ;;
    *list-units*)  printf 'spira-sentinel.service loaded active running Sentinel\n' ;;
    *restart*)     bash "${SPIRA_RELEASES}/current/spira/probe.sh" ;;
esac
exit "${MOCK_SC_EXIT:-0}"
EOF
chmod +x "$MOCK_SC"

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
        "UNIT_STATE_FILE=$UNIT_STATE_FILE" \
        "${extra_env[@]+"${extra_env[@]}"}" \
        bash "$ACTIVATE" "${activate_args[@]+"${activate_args[@]}"}" 2>&1
}

# Release names — timestamps chosen to sort cleanly (A < B lexicographically).
REL_A="spira-20260912T090000Z"
REL_B="spira-20260912T100000Z"

TB_A="$(make_tarball "$REL_A")"
TB_B="$(make_tarball "$REL_B")"

# ==========================================================================
echo
echo "STEP 1: activate A — establish initial release"
# ==========================================================================
_out="$(run_activate -- "$TB_A")"
_rc=$?
is0  "activate-A: exits 0" "$_rc"
_state="$(cat "$UNIT_STATE_FILE" 2>/dev/null || true)"
want "activate-A: unit executes A's code" "$REL_A" "$_state"

# ==========================================================================
echo
echo "STEP 2: activate B over A — positive control"
# The mid-state assertion genuinely fails if B's activation did not take:
# probe.sh follows current through the symlink, so if current still pointed
# at A the state file would contain A's path and the assertion below fails.
# ==========================================================================
_out="$(run_activate -- "$TB_B")"
_rc=$?
is0  "activate-B: exits 0" "$_rc"
_state="$(cat "$UNIT_STATE_FILE" 2>/dev/null || true)"
want "activate-B: unit executes B's code (positive control)" "$REL_B" "$_state"

# ==========================================================================
echo
echo "STEP 3: rollback — activate A again via the same activate.sh path"
# This is the property: rollback IS activation of the previous release.
# No separate rollback code path. activate.sh with A's tarball is the
# mechanism, in both directions.
# SEEN RED against unfixed tree: exits 1 "release already present".
# SEEN GREEN after fix: skips unpack (dir exists), swaps symlink, restarts.
# ==========================================================================
_out="$(run_activate -- "$TB_A")"
_rc=$?
is0  "rollback: exits 0" "$_rc"
_state="$(cat "$UNIT_STATE_FILE" 2>/dev/null || true)"
want "rollback: unit executes A's code" "$REL_A" "$_state"

# Confirm the mechanism: current symlink points back at A's release dir.
_cur="$(readlink "$RELEASES/current" 2>/dev/null || true)"
is "rollback: current symlink -> A" "$REL_A" "$_cur"

# ==========================================================================
echo
printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
