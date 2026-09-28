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
# 6. BINARY GUARD (sp-5dcpj). An already-enabled unit whose binary is missing is
#    disabled on the next ensure regardless of whether cargo is on PATH — the old
#    guard skipped the check entirely whenever cargo was present, which is exactly
#    "cargo present, build never run" — the case that produced 1500+ failed broker
#    ticks. Paired with a binary-present run that must NOT disable it. Also proven
#    reachable on a run where no unit file's content changed — the guard used to
#    sit after an early exit that skipped it in exactly that case.
# 7. PRODUCER GUARD (sp-5dcpj). spira-broker.timer is disabled whenever
#    spira_broker_producer_present is false, regardless of the binary or the
#    rendered file — the case BINARY GUARD does not cover.
#
# THE FIXTURE builds from the real installer (law-prefer-the-real-dependency).
# A mock systemctl records calls without touching systemd.
#
# tier: T1
# covers: systemd/unit-ensure.sh systemd/install.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

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
# Inject SC_LOG into the mock's environment. Matching the leading '$' matters: without
# it the redirect target becomes the unexpandable "$/tmp/.../sc.log" and every call's
# logging line fails silently — invisible until something greps SC_LOG for a call that
# should be present, since a grep for absence passes either way.
sed -i "s|\$SC_LOG|$SC_LOG|" "$TMP/sc"
chmod +x "$TMP/sc"

# A real, executable ExecStart target for spira-cockpit.service's @SPIRA_SUPERVISE_BIN@.
# _ENSURE_SUPERVISE overrides it per-call (the MISSING-TARGET case below points it at a
# path that does not exist).
printf '#!/bin/sh\nexec "$@"\n' > "$BIN/spira-supervise" && chmod +x "$BIN/spira-supervise"

# A real, executable czar-pass binary so the BINARY GUARD is quiet by default.
# _ENSURE_CZAR_PASS_BIN overrides it per-call (the BINARY GUARD case below points it
# at a path that does not exist).
printf '#!/bin/sh\n' > "$BIN/spira-czar-pass" && chmod +x "$BIN/spira-czar-pass"

# A fake cargo on PATH — the guard must fire on a missing binary regardless of
# cargo's presence (the case the old cargo-gated guard missed).
printf '#!/bin/sh\nexit 0\n' > "$BIN/cargo" && chmod +x "$BIN/cargo"

# ensure <args> — run unit-ensure.sh in a controlled environment.
# SPIRA_BROKER_BIN defaults to a path under $TMP that is never created, not to "" —
# conf.sh fills an empty SPIRA_BROKER_BIN via spira_bin, and SPIRA_REPO here derives to
# the real checkout, so an empty default would pick up a real built broker binary
# whenever one exists and falsely satisfy the BINARY GUARD's executable check.
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
        SPIRA_SUPERVISE_BIN="${_ENSURE_SUPERVISE:-$BIN/spira-supervise}" \
        SPIRA_CZAR_PASS_BIN="${_ENSURE_CZAR_PASS_BIN:-$BIN/spira-czar-pass}" \
        SPIRA_BROKER_BIN="${_ENSURE_BROKER_BIN:-$TMP/no-such-broker}" \
        SPIRA_BROKER_ENABLE="${_ENSURE_BROKER_ENABLE:-0}" \
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
    SPIRA_SUPERVISE_BIN="$BIN/spira-supervise" \
    bash "$HERE/../systemd/install.sh" --render 2>&1)"

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
[ -f "$DEST/spira-czar-pass-prod.timer" ] \
    && ok "fixture: czar-pass timer present" \
    || bad "fixture: czar-pass timer present" "spira-czar-pass-prod.timer missing"

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
# spira-cockpit.service's ExecStart is @SPIRA_SUPERVISE_BIN@, and unlike
# spira-loom/broker.service, units.sh does not gate its inclusion in UNITS/ENABLE
# on that binary's executability — so an unexecutable SPIRA_SUPERVISE_BIN reaches
# _ue_execstart_ok unchanged. It is the one core (non-OPTIONAL) unit through which
# the guard can be exercised end to end without editing UNITS/ENABLE from outside
# the script.
#
# Removing the installed file first makes ensure() see it as new — only new units
# reach the enable step (an updated/DIFFERS unit is left for the operator to
# restart, per unit-ensure.sh's own comment).
BAD_SUPERVISE="$TMP/nonexistent-supervise-binary"

rm -f "$DEST/spira-cockpit-prod.service"
: > "$SC_LOG"
_ENSURE_SUPERVISE="$BAD_SUPERVISE"
mt_out="$(ensure)"
mt_rc=$?
unset _ENSURE_SUPERVISE
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
echo "BINARY GUARD — enabled unit disabled when its binary is missing, cargo or not (sp-5dcpj):"
# ==========================================================================
# POSITIVE CONTROL FIRST (law-absence-needs-a-positive-control): drive a content
# change so the script passes the early no-op exit, with a fake cargo already on
# PATH (added above), and a missing czar-pass binary. The guard must disable the
# already-enabled timer despite cargo being present — the exact case the old
# `! command -v cargo` gate missed.
printf '\n# force a content change to re-enter the changed path (guard case)\n' >> "$czar_timer"
: > "$SC_LOG"
_ENSURE_CZAR_PASS_BIN="$TMP/nonexistent-czar-pass"
guard_out="$(ensure)"
unset _ENSURE_CZAR_PASS_BIN
want "BINARY GUARD: disables the enabled timer whose binary is missing" \
     "DISABLED spira-czar-pass-prod.timer" "$guard_out"
if grep -q "disable spira-czar-pass-prod.timer" "$SC_LOG"; then
    ok "BINARY GUARD: systemctl disable was actually called"
else
    bad "BINARY GUARD: systemctl disable was actually called" "$(cat "$SC_LOG")"
fi

# PAIR: same trigger, binary present — the guard must stay quiet.
printf '\n# force another content change (no-guard case)\n' >> "$czar_timer"
: > "$SC_LOG"
noguard_out="$(ensure)"
nowant "BINARY GUARD: does not disable when the binary is executable" \
     "DISABLED spira-czar-pass" "$noguard_out"

# ==========================================================================
echo
echo "BINARY GUARD — reachable on a run where nothing else changed (sp-5dcpj):"
# ==========================================================================
# The guard used to sit after an early exit that fired whenever no unit file's
# rendered content changed on the run — unreachable in the production case this
# bead describes, where landing this fix does not itself touch any unit template.
# No content edit here; only the binary breaks.
: > "$SC_LOG"
_ENSURE_CZAR_PASS_BIN="$TMP/nonexistent-czar-pass"
zero_change_out="$(ensure)"
unset _ENSURE_CZAR_PASS_BIN
want "BINARY GUARD: run itself reports no changes" "no changes" "$zero_change_out"
want "BINARY GUARD: still disables the unit on that same run" \
     "DISABLED spira-czar-pass-prod.timer" "$zero_change_out"

# ==========================================================================
echo
echo "PRODUCER GUARD — spira-broker.timer needs the producer opt-in, disabled without it (sp-5dcpj):"
# ==========================================================================
printf '#!/bin/sh\n' > "$BIN/spira-broker" && chmod +x "$BIN/spira-broker"

# POSITIVE CONTROL: producer present (SPIRA_BROKER_ENABLE=1) + binary executable —
# the timer is installed and enabled.
_ENSURE_BROKER_BIN="$BIN/spira-broker"
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
unset _ENSURE_BROKER_ENABLE _ENSURE_BROKER_BIN
nowant "PRODUCER GUARD: does not disable when producer present" \
     "DISABLED spira-broker" "$prod_again_out"

# ==========================================================================
tl_summary
