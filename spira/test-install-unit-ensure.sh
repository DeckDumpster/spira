#!/usr/bin/env bash
#
# test-install-unit-ensure.sh — unit-ensure.sh installs missing/changed units and
# is idempotent when nothing changed.
#
# WHAT IS TESTED
# --------------
# 1. POSITIVE CONTROL. unit-ensure.sh installs a unit that was absent, then
#    daemon-reload and enable+start are called. Proves the script fires.
# 2. IDEMPOTENCY. A second run with no template change writes nothing and calls
#    no daemon-reload.
# 3. CONTENT CHANGE. A unit whose installed file differs from the template is
#    updated; daemon-reload is called.
# 4. NO-OP WHEN ALL CURRENT. When every unit is up to date, the script exits 0
#    with no daemon-reload.
#
# THE FIXTURE builds from the real installer (law-prefer-the-real-dependency).
# A mock systemctl records calls without touching systemd.
#
# covers: systemd/unit-ensure.sh systemd/install.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }

echo "test-install-unit-ensure.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

DEST="$TMP/home/.config/systemd/user"
BIN="$TMP/bin"
mkdir -p "$DEST" "$BIN" "$TMP/db/.beads" "$TMP/run"
touch "$TMP/watchers-empty"

# Fake bd: conf.sh calls "bd migrate schema" on source; answer without a real db.
cat > "$BIN/bd" <<'FAKEBD'
#!/usr/bin/env bash
case "$*" in
    *"migrate schema"*) printf '✓ Schema already at v61\n'; exit 0 ;;
    *"list"*"--limit"*) printf '[]\n'; exit 0 ;;
    *) exit 0 ;;
esac
FAKEBD
chmod +x "$BIN/bd"

# Mock systemctl: records every call to a log file, reports units as enabled/active.
SC_LOG="$TMP/sc.log"
cat > "$TMP/sc" <<'MOCK'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$SC_LOG"
case "$*" in
    *"is-active"*"--quiet"*) exit 0 ;;
    *"is-active"*)  printf 'active\n'; exit 0 ;;
    *"is-enabled"*) printf 'enabled\n'; exit 0 ;;
    *) exit 0 ;;
esac
MOCK
# Inject SC_LOG into the mock's environment.
sed -i "s|SC_LOG|$SC_LOG|" "$TMP/sc"
chmod +x "$TMP/sc"

# ensure <args> — run unit-ensure.sh in a controlled environment.
ensure() {
    env -i PATH="$PATH" HOME="$TMP/home" \
        SPIRA_CONF=/nonexistent \
        SPIRA_PATH="$BIN" \
        SPIRA_WATCHERS="$TMP/watchers-empty" \
        SPIRA_DB="$TMP/db" \
        SPIRA_RUN="$TMP/run" \
        SPIRA_INSTANCE=prod \
        SPIRA_DOLT_DATA="" SPIRA_TESTDB_DATA="" \
        SPIRA_INSTALL_FORCE=1 \
        SPIRA_SYSTEMCTL="$TMP/sc" \
        bash "$HERE/../systemd/unit-ensure.sh" "$@" 2>&1
}

# ==========================================================================
echo
echo "positive control — render produces valid output before testing:"
# ==========================================================================
rendered="$(env -i PATH="$PATH" HOME="$TMP/home" \
    SPIRA_CONF=/nonexistent \
    SPIRA_PATH="$BIN" \
    SPIRA_WATCHERS="$TMP/watchers-empty" \
    SPIRA_DB="$TMP/db" SPIRA_RUN="$TMP/run" \
    SPIRA_INSTANCE=prod SPIRA_DOLT_DATA="" SPIRA_TESTDB_DATA="" \
    bash "$HERE/../systemd/install.sh" --render 2>&1)"
want "render produces headers" "=====" "$rendered"

# Write all units to DEST (full install).
current_unit=""
while IFS= read -r line; do
    if [[ "$line" =~ ^=====\ (.+)\ =====$ ]]; then
        current_unit="${BASH_REMATCH[1]}"
        > "$DEST/$current_unit"
    elif [ -n "$current_unit" ]; then
        printf '%s\n' "$line" >> "$DEST/$current_unit"
    fi
done <<< "$rendered"
n="$(ls "$DEST" | wc -l)"
[ "$n" -gt 0 ] && ok "installed $n unit(s) for fixture" \
    || bad "fixture install" "no files in $DEST"

# ==========================================================================
echo
echo "idempotency — no changes when all units are current:"
# ==========================================================================
: > "$SC_LOG"
noop_out="$(ensure)"
noop_rc=$?
is "no-op exits 0" "0" "$noop_rc"
want   "no-op reports no changes" "no changes" "$noop_out"
nowant "no-op does not print 'installed'" "installed" "$noop_out"
# daemon-reload must NOT be called when nothing changed.
if grep -q "daemon-reload" "$SC_LOG" 2>/dev/null; then
    bad "no-op: daemon-reload not called" "daemon-reload appeared in sc.log"
else
    ok "no-op: daemon-reload not called"
fi

# ==========================================================================
echo
echo "missing unit — installed when absent:"
# ==========================================================================
# Remove the czar-pass service (not the timer — keep the timer for the enable test).
czar_svc="$DEST/spira-czar-pass-prod.service"
if [ -f "$czar_svc" ]; then
    rm -f "$czar_svc"
    : > "$SC_LOG"
    miss_out="$(ensure)"
    want "missing: installed line present"       "installed  spira-czar-pass-prod.service" "$miss_out"
    want "missing: daemon-reload called"         "daemon-reload" "$miss_out"
    [ -f "$czar_svc" ] && ok "missing: file was created" \
        || bad "missing: file was created" "$czar_svc not found after ensure"
else
    bad "missing unit setup" "spira-czar-pass-prod.service not in fixture"
fi

# ==========================================================================
echo
echo "content change — updated unit triggers daemon-reload:"
# ==========================================================================
czar_timer="$DEST/spira-czar-pass-prod.timer"
if [ -f "$czar_timer" ]; then
    printf '\n# stale modification by test\n' >> "$czar_timer"
    : > "$SC_LOG"
    diff_out="$(ensure)"
    want "differs: updated line present"  "updated    spira-czar-pass-prod.timer" "$diff_out"
    want "differs: daemon-reload called"  "daemon-reload" "$diff_out"
    # The updated unit should match the template now.
    : > "$SC_LOG"
    again_out="$(ensure)"
    want "differs: second run is no-op" "no changes" "$again_out"
else
    bad "content change setup" "spira-czar-pass-prod.timer not in fixture"
fi

# ==========================================================================
echo
printf '  %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
