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
# 5. MISSING-TARGET (gap G10). A newly-installed unit whose ExecStart is not
#    executable is refused, not enabled, via unit-ensure.sh's own
#    _ue_execstart_ok — not a copy of it.
# 6. PRODUCER GUARD (sp-5dcpj). spira-broker.timer is disabled whenever
#    spira_broker_producer_present is false, regardless of the rendered file.
#
# The BINARY GUARD cases (an enabled unit whose binary is missing) are deleted with the
# guard itself (sp-gypjk): a release always carries every binary.
#
# THE FIXTURE builds from the real installer (law-prefer-the-real-dependency).
# A mock systemctl records calls without touching systemd.
#
# tier: T1
# covers: install/src/bin/unit_ensure.rs install/src/bin/units_install.rs UC-instance-lifecycle-30
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
. "$HERE/lib-test-install.sh"

echo "test-install-unit-ensure.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

DEST="$TMP/home/.config/systemd/user"
BIN="$TMP/bin"
mkdir -p "$DEST" "$BIN" "$TMP/db/.beads" "$TMP/run"

# Pin the binaries under test to a private release root. Resolved through PATH they sit
# beside the host's live `current` symlink, and unit-ensure refuses (exit 1) whenever that
# symlink names another release — a verdict the host decides, not the code.
PIN="$TMP/pinned-release"
mkdir -p "$PIN/bin"
for t in unit-ensure units-install; do
    src="$(command -v "$t")" || { echo "test-install-unit-ensure.sh: $t not on PATH" >&2; exit 1; }
    cp -L "$src" "$PIN/bin/$t"
done
ln -s "$HERE/../systemd" "$PIN/systemd"
ln -s "$HERE" "$PIN/spira"
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
# Inject SC_LOG into the mock's environment. Matching the leading '$' matters: without
# it the redirect target becomes the unexpandable "$/tmp/.../sc.log" and every call's
# logging line fails silently — invisible until something greps SC_LOG for a call that
# should be present, since a grep for absence passes either way.
sed -i "s|\$SC_LOG|$SC_LOG|" "$TMP/sc"
chmod +x "$TMP/sc"

# A release-shaped prod root (sp-gypjk): units ExecStart <root>/bin/<tool>, so the
# spira-cockpit.service ExecStart is $PROD_ROOT/bin/spira-supervise. _ENSURE_PROD overrides it
# per-call — the MISSING-TARGET case below points it at a root whose bin/spira-supervise is gone.
PROD="$(install_fixture_prod "$TMP/prod" "$HERE")"
BADPROD="$(install_fixture_prod "$TMP/badprod" "$HERE")"
rm -f "$TMP/badprod/bin/spira-supervise"


# ensure <args> — run unit-ensure.sh in a controlled environment. The mocks go first on
# the caller's PATH (conf.sh keeps it first and only appends SPIRA_PATH).
ensure() {
    tl_config SPIRA_PATH="$BIN" SPIRA_WATCHERS="$TMP/watchers-empty" SPIRA_DB="$TMP/db" \
        SPIRA_RUN="$TMP/run" SPIRA_INSTANCE=prod SPIRA_DOLT_DATA="" SPIRA_TESTDB_DATA="" \
        SPIRA_PROD="${_ENSURE_PROD:-$PROD}" SPIRA_BROKER_ENABLE="${_ENSURE_BROKER_ENABLE:-0}" \
        SPIRA_BD=""
    # SPIRA_BROKER_ENABLE ALSO AS PLAIN ENV: install/src/ensure.rs's producer guard still
    # reads it with a bare std::env::var, never through spira_config::process::cfg (unlike
    # the manifest's own ENABLE-set resolution a few lines over) — tl_config's declaration
    # never reaches that one check.
    env -i PATH="$PIN/bin:$BIN:$PATH" HOME="$TMP/home" \
        SPIRA_TOML="$SPIRA_TOML" SPIRA_HOME="$PIN/spira" \
        SPIRA_CONF=/nonexistent \
        SPIRA_INSTALL_FORCE=1 \
        SPIRA_SYSTEMCTL="$TMP/sc" \
        SPIRA_BROKER_ENABLE="${_ENSURE_BROKER_ENABLE:-0}" \
        unit-ensure "$@" 2>&1
}

# ==========================================================================
echo
echo "positive control — render produces valid output before testing:"
# ==========================================================================
tl_config SPIRA_PATH="$BIN" SPIRA_WATCHERS="$TMP/watchers-empty" SPIRA_DB="$TMP/db" \
    SPIRA_RUN="$TMP/run" SPIRA_INSTANCE=prod SPIRA_DOLT_DATA="" SPIRA_TESTDB_DATA="" \
    SPIRA_PROD="$PROD" SPIRA_BD=""
rendered="$(env -i PATH="$PIN/bin:$BIN:$PATH" HOME="$TMP/home" \
    SPIRA_TOML="$SPIRA_TOML" SPIRA_HOME="$PIN/spira" \
    SPIRA_CONF=/nonexistent \
    units-install --render 2>&1)"

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
    || bad "fixture install" "no files in $DEST: ${rendered:0:600}"
[ -f "$DEST/spira-czar-pass-prod.timer" ] \
    && ok "fixture: czar-pass timer present" \
    || bad "fixture: czar-pass timer present" "spira-czar-pass-prod.timer missing"

# ==========================================================================
echo
echo "pinned release — host release state cannot decide the verdict:"
# ==========================================================================
[ ! -e "$PIN/current" ] && ok "pinned release has no sibling 'current' symlink" \
    || bad "pinned release has no sibling 'current' symlink" "$PIN/current exists"
# POSITIVE CONTROL: the hazard is real — a copy beside a current naming another release refuses.
SK="$TMP/skew"; mkdir -p "$SK/aaa/bin" "$SK/bbb"
cp "$PIN/bin/unit-ensure" "$SK/aaa/bin/unit-ensure"; ln -s bbb "$SK/current"
tl_config SPIRA_DB="$TMP/db" SPIRA_RUN="$TMP/run" SPIRA_INSTANCE=prod SPIRA_BD=""
skew_out="$(env -i PATH="$SK/aaa/bin:$BIN:$PATH" HOME="$TMP/home" SPIRA_TOML="$SPIRA_TOML" SPIRA_HOME="$PIN/spira" SPIRA_CONF=/nonexistent \
    SPIRA_SYSTEMCTL="$TMP/sc" \
    unit-ensure 2>&1)"
want "skewed release copy is refused" "REFUSING to write units" "$skew_out"
nowant "pinned run is not refused" "REFUSING" "$(ensure)"

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
echo "MISSING-TARGET — unit with absent ExecStart binary is not enabled (real script, real template):"
# ==========================================================================
# GAP G10: this drives the REAL unit-ensure.sh script and _ue_execstart_ok
# against a REAL template — not a reimplementation of the guard, which would
# pass even if the guard in unit-ensure.sh were deleted.
#
# spira-cockpit.service's ExecStart is @SPIRA_PROD_ROOT@/bin/spira-supervise, a core unit
# units.sh always installs — so a prod root without that binary reaches _ue_execstart_ok
# unchanged, exercising the guard end to end without editing UNITS/ENABLE from outside.
#
# Removing the installed file first makes ensure() see it as new — only new units
# reach the enable step (an updated/DIFFERS unit is left for the operator to
# restart, per unit-ensure.sh's own comment).
BAD_SUPERVISE="$TMP/badprod/bin/spira-supervise"

rm -f "$DEST/spira-cockpit-prod.service"
: > "$SC_LOG"
_ENSURE_PROD="$BADPROD"
mt_out="$(ensure)"
mt_rc=$?
unset _ENSURE_PROD
is   "MISSING-TARGET: ensure still exits 0 (a refused enable is not a script failure)" \
     "0" "$mt_rc"
want "MISSING-TARGET: absent ExecStart target produces MISSING-TARGET line" \
     "MISSING-TARGET  spira-cockpit-prod.service" "$mt_out"
want "MISSING-TARGET: names the unexecutable target" "$BAD_SUPERVISE" "$mt_out"
nowant "MISSING-TARGET: absent target is NOT enabled" \
     "enabled+started  spira-cockpit-prod.service" "$mt_out"
if grep -q "enable spira-cockpit-prod.service" "$SC_LOG"; then
    bad "MISSING-TARGET: systemctl enable is never called for the refused unit" \
        "enable appeared in sc.log: $(cat "$SC_LOG")"
else
    ok "MISSING-TARGET: systemctl enable is never called for the refused unit"
fi

# PAIR: same unit, new again, this time with a valid executable ExecStart target.
rm -f "$DEST/spira-cockpit-prod.service"
: > "$SC_LOG"
present_out="$(ensure)"
want "MISSING-TARGET: present ExecStart target is enabled" \
     "enabled+started  spira-cockpit-prod.service" "$present_out"

# ==========================================================================
echo
echo "PRODUCER GUARD — spira-broker.timer needs the producer opt-in, disabled without it (sp-5dcpj):"
# ==========================================================================
# POSITIVE CONTROL: producer present (SPIRA_BROKER_ENABLE=1) — the timer is installed and
# enabled.
_ENSURE_BROKER_ENABLE=1
: > "$SC_LOG"
broker_on_out="$(ensure)"
unset _ENSURE_BROKER_ENABLE
want "PRODUCER GUARD: broker timer enabled when producer present" \
     "enabled+started  spira-broker-prod.timer" "$broker_on_out"

# NO PRODUCER, NO CONTENT CHANGE: _ENSURE_BROKER_ENABLE unset now (defaults to 0,
# same as spira_broker_producer_present's default) and nothing about the rendered
# unit files depends on it, so this run has zero changes — the guard must still fire.
: > "$SC_LOG"
noprod_out="$(ensure)"
want "PRODUCER GUARD: run itself reports no changes" "no changes" "$noprod_out"
want "PRODUCER GUARD: disables the timer with no producer" \
     "DISABLED spira-broker-prod.timer (no producer" "$noprod_out"
if grep -q "disable spira-broker-prod.timer" "$SC_LOG"; then
    ok "PRODUCER GUARD: systemctl disable was actually called"
else
    bad "PRODUCER GUARD: systemctl disable was actually called" "$(cat "$SC_LOG")"
fi

# PAIR: producer present again — the guard must stay quiet.
_ENSURE_BROKER_ENABLE=1
: > "$SC_LOG"
prod_again_out="$(ensure)"
unset _ENSURE_BROKER_ENABLE
nowant "PRODUCER GUARD: does not disable when producer present" \
     "DISABLED spira-broker" "$prod_again_out"

# ==========================================================================
tl_summary
