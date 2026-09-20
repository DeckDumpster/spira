#!/usr/bin/env bash
#
# test-install-idempotent.sh — install.sh is idempotent: a second run with no template
# change writes no files and calls no daemon-reload; operator-disabled and masked units
# are left at their current state.
#
#   ./test-install-idempotent.sh
#
# THE PROPERTIES UNDER TEST
# -------------------------
# 1. IDEMPOTENCY: a second install with no template change writes no files and does not
#    call daemon-reload.
# 2. DISABLED: a unit the operator disabled stays disabled — install.sh does not re-enable
#    it (tested via the halted-world path, which calls enable unconditionally in the old code).
# 3. MASKED: a unit masked by the operator (symlink to /dev/null) is skipped and left
#    intact — the file is not overwritten.
# 4. POSITIVE CONTROL: a unit whose rendered template genuinely changed IS installed and
#    the install DOES call daemon-reload. Without this, idempotent and broken look identical.
# 5. SUMMARY: the install output names how many unit files changed.
#
# THE FIXTURE USES A MOCK systemctl THAT RECORDS CALLS WITHOUT TOUCHING SYSTEMD.
# Pin SPIRA_RUN to an isolated directory and use SPIRA_INSTALL_FORCE=1 to bypass
# the landref and live-aeon checks (law-gates-run-in-a-clean-environment).
#
# defect: sp-s6hk
# covers: systemd/install.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
REAL_REPO="$(cd "$HERE/.." && pwd -P)"
REAL_COCKPIT="$(cd "$HERE/../cockpit" && pwd -P)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }
iszero() { [ "$2" = 0 ] && ok "$1" || bad "$1" "wanted exit 0, got $2"; }

echo "test-install-idempotent.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

# ---------------------------------------------------------------------------
# Fixture: minimal harness tree.
# ---------------------------------------------------------------------------
FIXTURE="$TMP/harness"
mkdir -p "$FIXTURE/systemd" "$FIXTURE/spira"

for f in "$HERE/../systemd/"*.service "$HERE/../systemd/"*.timer; do
    [ -e "$f" ] || continue
    ln -s "$f" "$FIXTURE/systemd/$(basename "$f")"
done
ln -s "$HERE/../systemd/install.sh" "$FIXTURE/systemd/install.sh"
for f in conf.sh watchd.sh lib.sh; do
    [ -e "$HERE/$f" ] && ln -s "$HERE/$f" "$FIXTURE/spira/$f"
done

printf '# empty — test fixture\n' > "$FIXTURE/spira/watchers"
printf '# empty\n' > "$FIXTURE/spira/repo-map.example"

printf '#!/usr/bin/env bash\nexit 0\n' > "$FIXTURE/spira/install-session-hook.sh"
chmod +x "$FIXTURE/spira/install-session-hook.sh"

DEST="$TMP/home/.config/systemd/user"
SPIRA_RUN_DIR="$TMP/run"
MOCK_BIN="$TMP/mock-bin"
mkdir -p "$DEST" "$SPIRA_RUN_DIR" "$MOCK_BIN"
MOCK_LOG="$TMP/systemctl.log"

# MOCK_ENABLED_DIR: a directory where per-unit is-enabled state files live.
# Write "disabled" to $MOCK_ENABLED_DIR/<unit> to make that unit appear disabled.
# No file → "enabled" (the default).
MOCK_ENABLED_DIR="$TMP/enabled"
mkdir -p "$MOCK_ENABLED_DIR"

cat > "$MOCK_BIN/systemctl" <<'MOCK'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "${MOCK_LOG}"
case "$*" in
    *is-active*)
        printf '%s\n' "${MOCK_IS_ACTIVE:-active}"
        ;;
    *is-enabled*)
        # Last positional argument is the unit name.
        _u="${@: -1}"
        _sf="${MOCK_ENABLED_DIR:-}/$_u"
        if [ -n "${MOCK_ENABLED_DIR:-}" ] && [ -f "$_sf" ]; then
            cat "$_sf"
        else
            echo "enabled"
        fi
        ;;
    *"list-unit-files"*|*"list-units"*|*"list-timers"*)
        : # no output — empty manifest prune
        ;;
esac
exit 0
MOCK
chmod +x "$MOCK_BIN/systemctl"

printf '#!/usr/bin/env bash\nexit 0\n' > "$MOCK_BIN/loginctl"
chmod +x "$MOCK_BIN/loginctl"
printf '#!/usr/bin/env bash\nexit 0\n' > "$MOCK_BIN/spira-supervise"
chmod +x "$MOCK_BIN/spira-supervise"

inst() {
    > "$MOCK_LOG"
    env -i \
        "PATH=$PATH" \
        "HOME=$TMP/home" \
        SPIRA_CONF=/nonexistent \
        "SPIRA_PATH=$MOCK_BIN" \
        "SPIRA_WATCHERS=$FIXTURE/spira/watchers" \
        SPIRA_DOLT_DATA= \
        SPIRA_TESTDB_DATA= \
        "SPIRA_RUN=$SPIRA_RUN_DIR" \
        "SPIRA_HOME=$HERE" \
        "SPIRA_PROD=$HERE" \
        "SPIRA_REPO=$REAL_REPO" \
        "SPIRA_COCKPIT=$REAL_COCKPIT" \
        "SPIRA_SUPERVISE_BIN=$MOCK_BIN/spira-supervise" \
        "MOCK_LOG=$MOCK_LOG" \
        "MOCK_ENABLED_DIR=$MOCK_ENABLED_DIR" \
        "MOCK_IS_ACTIVE=${MOCK_IS_ACTIVE:-active}" \
        SPIRA_INSTALL_FORCE=1 \
        bash "$FIXTURE/systemd/install.sh" "$@" 2>&1
}

# Seed DEST with rendered units so baseline is "nothing changed".
rendered="$(inst --render 2>&1)"; rc=$?
if [ "$rc" != 0 ]; then
    printf 'fixture: install.sh --render failed (rc=%s) — cannot continue\n' "$rc"
    printf '%s\n' "$rendered"
    exit 1
fi
current_unit=""
while IFS= read -r line; do
    if [[ "$line" =~ ^=====\ (.+)\ =====$ ]]; then
        current_unit="${BASH_REMATCH[1]}"; > "$DEST/$current_unit"
    elif [ -n "$current_unit" ]; then
        printf '%s\n' "$line" >> "$DEST/$current_unit"
    fi
done <<< "$rendered"

# Pick a unit to use across tests.
test_unit="spira-sentinel-${SPIRA_INSTANCE:-prod}.timer"
[ -f "$DEST/$test_unit" ] || test_unit="$(ls "$DEST"/*.timer 2>/dev/null | head -1 | xargs basename)"
[ -f "$DEST/$test_unit" ] || { printf 'fixture: no timer found in DEST\n'; exit 1; }

# ==========================================================================
echo
echo "POSITIVE CONTROL — a changed unit file must be installed (daemon-reload IS called):"
# ==========================================================================
# Append a line so the unit in DEST differs from what install.sh would render.
printf '# deliberately altered for test\n' >> "$DEST/$test_unit"
ctrl_out="$(MOCK_IS_ACTIVE=active inst)"
ctrl_log="$(cat "$MOCK_LOG")"
ctrl_rc=$?
iszero  "positive-ctrl: exit 0 after installing changed unit"       "$ctrl_rc"
want    "positive-ctrl: output names the installed unit"            "installed $test_unit" "$ctrl_out"
want    "positive-ctrl: daemon-reload IS called when content changed" "daemon-reload" "$ctrl_log"
want    "positive-ctrl: summary reports at least 1 changed"         "1 unit" "$ctrl_out"

# After install, DEST matches rendered content again (install wrote the correct content).

# ==========================================================================
echo
echo "IDEMPOTENCY — second run with no template change must not write files or reload:"
# ==========================================================================
noop_out="$(MOCK_IS_ACTIVE=active inst)"
noop_log="$(cat "$MOCK_LOG")"
noop_rc=$?
iszero  "idempotent: exit 0 on second run"                          "$noop_rc"
nowant  "idempotent: no 'installed' line in output"                 "installed " "$noop_out"
nowant  "idempotent: daemon-reload NOT called when nothing changed"  "daemon-reload" "$noop_log"
want    "idempotent: reports unchanged units"                       "unchanged" "$noop_out"
want    "idempotent: summary reports 0 changed"                     "0 unit" "$noop_out"

# ==========================================================================
echo
echo "MASKED — a unit masked by the operator must not be overwritten:"
# ==========================================================================
rm -f "$DEST/$test_unit"
ln -s /dev/null "$DEST/$test_unit"
mask_out="$(MOCK_IS_ACTIVE=active inst)"
mask_log="$(cat "$MOCK_LOG")"
# Verify the symlink survived — install.sh must not have overwritten it.
is "masked: symlink to /dev/null still intact after install" \
    "$(readlink "$DEST/$test_unit" 2>/dev/null)" "/dev/null"
want   "masked: output reports unit as masked"                       "masked" "$mask_out"
nowant "masked: daemon-reload not called (masked unit is not a change)" "daemon-reload" "$mask_log"
# Positive control: removing the mask lets install write the unit normally.
rm -f "$DEST/$test_unit"
restore_out="$(MOCK_IS_ACTIVE=active inst)"
want "masked-restore: unit installed after mask removed" "installed $test_unit" "$restore_out"
# DEST is clean again.

# ==========================================================================
echo
echo "DISABLED — operator-disabled unit must not be re-enabled (halted-world path):"
# ==========================================================================
# The halted-world path calls `systemctl enable $u` for every unit in ENABLE —
# unconditionally, without is-active checks. So it is the sharpest test of whether
# a disable is respected: the old code re-enables regardless; the fixed code must not.
printf '%s\nwhy: test\n' "$(date -u '+%Y-%m-%dT%H:%M:%SZ')" > "$SPIRA_RUN_DIR/world.halted"
printf 'disabled\n' > "$MOCK_ENABLED_DIR/$test_unit"
dis_out="$(inst)"
dis_log="$(cat "$MOCK_LOG")"
# Verify enable was NOT called for this specific unit.
nowant "disabled: enable not called for the disabled unit" "enable $test_unit" "$dis_log"
want   "disabled: output notes the disabled unit was left unchanged" "disabled by operator" "$dis_out"
rm -f "$SPIRA_RUN_DIR/world.halted" "$MOCK_ENABLED_DIR/$test_unit"

# ==========================================================================
echo
printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" = 0 ]
