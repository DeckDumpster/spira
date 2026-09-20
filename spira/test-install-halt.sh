#!/usr/bin/env bash
#
# test-install-halt.sh — install.sh respects world.halted and does not restart a
# deliberately stopped Spira as a side effect of shipping a new unit.
#
#   ./test-install-halt.sh
#
# THE PROPERTY UNDER TEST
# -----------------------
# A halted world has $SPIRA_RUN/world.halted on disk. install.sh must install and
# enable units but must NOT start them. A running world must still get enable --now
# so the guard cannot degrade into "never start anything" (law-absence-needs-a-positive-control).
#
# THE FIXTURE USES A MOCK systemctl THAT RECORDS CALLS WITHOUT TOUCHING SYSTEMD.
# Assertions read the log to check which flags were used. A mock that exits 0 for
# every verb is permissive by design: we are testing the CALLER'S logic, not systemd's.
# Pin a non-default SPIRA_RUN so the halt stamp resolves there, not to the operator's
# live run directory (law-gates-run-in-a-clean-environment).
#
# defect: sp-8jy7
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

echo "test-install-halt.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

# ---------------------------------------------------------------------------
# Fixture: a minimal harness tree mirroring what test-unit-drift.sh builds.
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

# Stub so install.sh does not attempt to write real Claude Code settings.
printf '#!/usr/bin/env bash\nexit 0\n' > "$FIXTURE/spira/install-session-hook.sh"
chmod +x "$FIXTURE/spira/install-session-hook.sh"

DEST="$TMP/home/.config/systemd/user"
mkdir -p "$DEST"

# SPIRA_RUN is an isolated directory. The halt stamp lives here.
SPIRA_RUN="$TMP/run"
mkdir -p "$SPIRA_RUN"

# Mock binaries: record calls, do not touch real systemd.
MOCK_BIN="$TMP/mock-bin"
mkdir -p "$MOCK_BIN"
MOCK_LOG="$TMP/systemctl.log"

cat > "$MOCK_BIN/systemctl" <<'MOCK'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "${MOCK_LOG}"
case "$*" in
    *is-active*) echo "inactive" ;;
esac
exit 0
MOCK
chmod +x "$MOCK_BIN/systemctl"

printf '#!/usr/bin/env bash\nexit 0\n' > "$MOCK_BIN/loginctl"
chmod +x "$MOCK_BIN/loginctl"
printf '#!/usr/bin/env bash\nexit 0\n' > "$MOCK_BIN/spira-supervise"
chmod +x "$MOCK_BIN/spira-supervise"

# The installer, run in a controlled, minimal environment.
# SPIRA_PATH is used rather than prepending PATH directly because conf.sh resets PATH
# to "${SPIRA_PATH:+$SPIRA_PATH:}$HOME/.local/bin:/usr/local/bin:/usr/bin:/bin" —
# prepending to the outer PATH is silently overwritten (law-gates-run-in-a-clean-environment).
inst() {
    > "$MOCK_LOG"
    env -i PATH="$PATH" HOME="$TMP/home" \
        SPIRA_CONF=/nonexistent \
        SPIRA_PATH="$MOCK_BIN" \
        SPIRA_WATCHERS="$FIXTURE/spira/watchers" \
        SPIRA_DOLT_DATA="" \
        SPIRA_TESTDB_DATA="" \
        SPIRA_RUN="$SPIRA_RUN" \
        "SPIRA_HOME=$HERE" \
        "SPIRA_PROD=$HERE" \
        "SPIRA_REPO=$REAL_REPO" \
        "SPIRA_COCKPIT=$REAL_COCKPIT" \
        "SPIRA_SUPERVISE_BIN=$MOCK_BIN/spira-supervise" \
        MOCK_LOG="$MOCK_LOG" \
        SPIRA_INSTALL_FORCE=1 \
        bash "$FIXTURE/systemd/install.sh" "$@" 2>&1
}

# Populate DEST with units so the installer does not fail on missing rendered targets.
rendered="$(inst --render 2>&1)"; rc=$?
if [ "$rc" != 0 ]; then
    echo "fixture: install.sh --render failed (rc=$rc) — cannot continue"
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

# ==========================================================================
echo
echo "RUNNING world — install.sh starts units with enable --now:"
# ==========================================================================
running_out="$(inst)"
running_log="$(cat "$MOCK_LOG")"
want "running: enable --now appears in systemctl calls" "--now" "$running_log"
nowant "running: HALTED banner absent" "HALTED" "$running_out"

# ==========================================================================
echo
echo "HALTED world — install.sh enables but does not start units:"
# ==========================================================================
{ date -u '+%Y-%m-%dT%H:%M:%SZ'; printf 'why: test fixture\n'; } > "$SPIRA_RUN/world.halted"
halted_out="$(inst)"
halted_log="$(cat "$MOCK_LOG")"
want   "halted: output carries HALTED banner"         "HALTED"  "$halted_out"
want   "halted: output carries reason"                "why:"    "$halted_out"
want   "halted: enable appears in systemctl calls"    "enable"  "$halted_log"
nowant "halted: enable --now does NOT appear"         "enable --now"   "$halted_log"
rm -f "$SPIRA_RUN/world.halted"

# ==========================================================================
echo
printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" = 0 ]
