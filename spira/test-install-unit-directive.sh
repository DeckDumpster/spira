#!/usr/bin/env bash
#
# test-install-unit-directive.sh — rendered spira-*.timer files carry the
# correct per-instance Unit= directive; _migrate_legacy disables the old
# spira-watch@ template-instantiation form.
#
#   ./test-install-unit-directive.sh
#
# PROPERTIES UNDER TEST
# ---------------------
# 1. TIMER UNIT DIRECTIVE: every rendered spira-*-<instance>.timer carries
#    Unit=spira-*-<instance>.service, not the plain-named (un-suffixed) service.
#    Without this fix, a timer installed as spira-sentinel-prod.timer still
#    carries Unit=spira-sentinel.service in its body, so it fires the legacy
#    service rather than the per-instance one — and on the next install the
#    test timer fires the prod service.
# 2. MIGRATE TEMPLATE INSTANCES: _migrate_legacy disables
#    spira-watch@<name>.service (the systemd template-instantiation form) in
#    addition to the plain-hyphen form. Before per-instance naming, watchers
#    were started via the spira-watch@.service template; the running units were
#    spira-watch@answers.service (@ not hyphen), so a migration that only
#    retires the hyphen form leaves the old instance holding the work, causing
#    the new per-instance unit to crash-loop against it.
#
# ASSERTIONS ARE FROM RENDERED FILES, NOT TEMPLATES. The templates are the
# source; the defect is in the rendered output. Asserting against the template
# source catches nothing: the template was always correct — it just wasn't
# being post-processed.
#
# SCAR: the fixture's cp list omitted suite-covers.sh after sp-dt8u added it
# to lib.sh; lib.sh failed at source time before any assertion ran. A real
# install carries no fixture and the cause cannot exist.
#
# SKIP CONDITION: XDG_RUNTIME_DIR is not /run/user/1001 (suite must run inside
# the testenv container as spirauser) or user systemd is not responding.
#
# covers: systemd/install.sh
# covers: systemd/spira-*.timer
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
pass=0; fail=0
ok()      { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()     { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want()    { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant()  { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }
iszero()  { [ "$2" = 0 ] && ok "$1" || bad "$1" "wanted exit 0, got $2"; }

echo "test-install-unit-directive.sh"

[ "${XDG_RUNTIME_DIR:-}" = "/run/user/1001" ] || {
    printf 'SKIP test-install-unit-directive.sh: not running as spirauser inside testenv container\n' >&2
    exit 77
}
# hermetic-ok: SKIP check — exits 77 when not inside the testenv container
systemctl --user status >/dev/null 2>&1 || {
    printf 'SKIP test-install-unit-directive.sh: user systemd not running inside container\n' >&2
    exit 77
}

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

SPIRA_RUN_DIR="$TMP/run"
mkdir -p "$SPIRA_RUN_DIR"
touch "$SPIRA_RUN_DIR/world.halted"

# ==========================================================================
echo
echo "TIMER UNIT DIRECTIVE — rendered spira-*-test.timer carries Unit=...-test.service:"
# ==========================================================================

rendered="$(
    SPIRA_CONF=/nonexistent \
    SPIRA_RUN="$SPIRA_RUN_DIR" \
    SPIRA_DOLT_DATA= SPIRA_TESTDB_DATA= \
    SPIRA_PROD= SPIRA_REPO_MAP=/nonexistent \
    SPIRA_INSTALL_FORCE=1 \
    bash "$HERE/../systemd/install.sh" test --render 2>&1
)"
render_rc=$?
iszero "render: --render exits 0" "$render_rc"

timer_count=0
timer_bad=0
current_unit=""
current_body=""
check_unit() {
    local uname="$1" body="$2"
    case "$uname" in
        spira-*-test.timer)
            timer_count=$((timer_count+1))
            while IFS= read -r ln; do
                case "$ln" in
                    Unit=spira-*-test.service) : ;;
                    Unit=*)
                        bad "unit directive: $uname carries wrong Unit= line: $ln" ""
                        timer_bad=$((timer_bad+1))
                        ;;
                esac
            done <<< "$body"
            ;;
    esac
}

while IFS= read -r line; do
    if [[ "$line" =~ ^=====\ (.+)\ =====$ ]]; then
        if [ -n "$current_unit" ]; then
            check_unit "$current_unit" "$current_body"
        fi
        current_unit="${BASH_REMATCH[1]}"
        current_body=""
    elif [ -n "$current_unit" ]; then
        current_body="${current_body}${line}"$'\n'
    fi
done <<< "$rendered"
[ -n "$current_unit" ] && check_unit "$current_unit" "$current_body"

if [ "$timer_count" -ge 10 ]; then
    ok "unit directive: at least 10 spira-*-test.timer files checked"
else
    bad "unit directive: expected >= 10 timers, got $timer_count" ""
fi
[ "$timer_bad" -eq 0 ] \
    && ok "unit directive: all checked timers carry the correct -test.service target" \
    || bad "unit directive: $timer_bad timer(s) point at a wrong service name" ""

want "unit directive: spira-sentinel-test.timer has Unit=spira-sentinel-test.service" \
     "Unit=spira-sentinel-test.service" "$rendered"

in_spira_test_timer=""
while IFS= read -r line; do
    if [[ "$line" =~ ^=====\ (spira-.*-test\.timer)\ =====$ ]]; then
        in_spira_test_timer=1
    elif [[ "$line" =~ ^=====\ .*\ =====$ ]]; then
        in_spira_test_timer=""
    elif [ -n "$in_spira_test_timer" ]; then
        case "$line" in
            Unit=spira-*.service)
                case "$line" in
                    *-test.service) : ;;
                    *) bad "unit directive: plain Unit= target found in a -test.timer: $line" "" ;;
                esac
                ;;
        esac
    fi
done <<< "$rendered"
ok "unit directive: no spira-*-test.timer carries a plain (un-suffixed) Unit= target"

case "$rendered" in
    *"Unit=beads-push-test.service"*)
        bad "unit directive: beads-push.timer wrongly got -test suffix on Unit=" "" ;;
    *)
        ok "unit directive: beads-push.timer Unit= unchanged (shared unit)" ;;
esac
case "$rendered" in
    *"Unit=cockpit-ensure-test.service"*)
        bad "unit directive: cockpit-ensure.timer wrongly got -test suffix on Unit=" "" ;;
    *)
        ok "unit directive: cockpit-ensure.timer Unit= unchanged (shared unit)" ;;
esac

# ==========================================================================
echo
echo "MIGRATE TEMPLATE INSTANCES — _migrate_legacy disables spira-watch@<name>.service:"
# ==========================================================================

# Pre-install legacy unit files so systemctl disable returns 0.
# The hyphen form: spira-watch-answers.service
# The @ template-instance form: spira-watch@answers.service (requires the
# template file spira-watch@.service to exist in the unit search path).
UNITDIR="$HOME/.config/systemd/user"
mkdir -p "$UNITDIR"

printf '[Unit]\nDescription=legacy watcher (hyphen form)\n[Service]\nExecStart=/bin/true\n[Install]\nWantedBy=default.target\n' \
    > "$UNITDIR/spira-watch-answers.service"
printf '[Unit]\nDescription=legacy watcher template\n[Service]\nExecStart=/bin/true\n[Install]\nWantedBy=default.target\n' \
    > "$UNITDIR/spira-watch@.service"
# Thin pass-through logger with enable/disable tracking. Records every systemctl
# call to MIGRATE_LOG and tracks enabled unit names in MIGRATE_LOG.enabled so that
# disable --now returns 0 for explicitly-enabled units regardless of real systemd
# state. In parallel-batch mode each suite gets its own HOME but the user daemon is
# bound to the container's real HOME, so `systemctl enable` on a unit file in the
# per-suite HOME may silently fail; the tracking file makes the disable deterministic.
MIGRATE_LOG="$TMP/migrate.log"
export MIGRATE_LOG
mkdir -p "$TMP/bin"
cat > "$TMP/bin/systemctl" << 'SCTL'
#!/bin/sh
printf '%s\n' "$*" >> "$MIGRATE_LOG"
_ENABLED="${MIGRATE_LOG}.enabled"
case "$*" in
    "--user enable "*)
        _u="${*#*--user enable }"; printf '%s\n' "$_u" >> "$_ENABLED"
        exec /usr/bin/systemctl "$@" ;;
    "--user disable --now "*)
        _u="${*#*--user disable --now }"
        if grep -qxF "$_u" "$_ENABLED" 2>/dev/null; then
            grep -vxF "$_u" "$_ENABLED" 2>/dev/null > "${_ENABLED}.tmp" \
                && mv "${_ENABLED}.tmp" "$_ENABLED" 2>/dev/null || true
            /usr/bin/systemctl "$@" 2>/dev/null || true; exit 0
        fi
        exec /usr/bin/systemctl "$@" ;;
    *) exec /usr/bin/systemctl "$@" ;;
esac
SCTL
chmod +x "$TMP/bin/systemctl"

# hermetic-ok: container-first suite — pre-plants legacy units in real systemd; SKIP guard exits 77
"$TMP/bin/systemctl" --user daemon-reload
"$TMP/bin/systemctl" --user enable spira-watch-answers.service 2>/dev/null || true  # hermetic-ok: container-first
"$TMP/bin/systemctl" --user enable "spira-watch@answers.service" 2>/dev/null || true  # hermetic-ok: container-first

WATCHERS="$TMP/watchers"
printf 'answers|daemon|/bin/true\n' > "$WATCHERS"

> "$MIGRATE_LOG"
migrate_out="$(
    SPIRA_CONF=/nonexistent \
    SPIRA_PATH="$TMP/bin" \
    SPIRA_RUN="$SPIRA_RUN_DIR" \
    SPIRA_WATCHERS="$WATCHERS" \
    SPIRA_DOLT_DATA= SPIRA_TESTDB_DATA= \
    SPIRA_PROD= SPIRA_REPO_MAP=/nonexistent \
    SPIRA_INSTALL_FORCE=1 \
    MIGRATE_LOG="$MIGRATE_LOG" \
    bash "$HERE/../systemd/install.sh" test 2>&1
)"
migrate_rc=$?
migrate_log="$(cat "$MIGRATE_LOG")"

iszero "migrate @-form: exit 0" "$migrate_rc"
want "migrate @-form: disable called on spira-watch@answers.service" \
     "spira-watch@answers.service" "$migrate_log"
want "migrate @-form: migrated line for @-form unit" \
     "spira-watch@answers.service" "$migrate_out"
want "migrate @-form: disable still called on hyphen form" \
     "spira-watch-answers.service" "$migrate_log"

# Clean up the legacy unit files.
rm -f "$UNITDIR/spira-watch-answers.service" "$UNITDIR/spira-watch@.service"
# hermetic-ok: container-first suite — reloads real systemd after cleanup; SKIP guard exits 77
systemctl --user daemon-reload 2>/dev/null || true

# ==========================================================================
echo
printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" = 0 ]
