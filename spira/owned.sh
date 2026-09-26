#!/usr/bin/env bash
# spira/owned.sh — declare and verify what one Spira installation owns outside its checkout.
#
#   owned.sh list  [<instance>]   print manifest: kind|id|location|phase|retention
#   owned.sh check [<instance>]   verify each artifact: kind|id|location|status
#                                 status: present | absent | drifted
#
# KINDS DECLARED
# --------------
#   unit          installed systemd unit files (~/.config/systemd/user/<name>)
#   linger        loginctl linger enabled for this user
#   runtime-tree  SPIRA_RUN directory
#   database      SPIRA_DB path
#   session-hook  SessionStart and PostCompact entries in SPIRA_CLIENT_SETTINGS
#   binary        built binaries: SPIRA_LOOM_BIN, SPIRA_PANEL
#   dolt-yaml     dolt-server.yaml configs in SPIRA_DOLT_DATA / SPIRA_TESTDB_DATA
#   cockpit-pane  tmux panes tagged @cockpit=panel and @cockpit=health
#   alert-dropin  50-spira-intake.conf drop-ins for alert units
#
# TEST SEAMS
# ----------
# SPIRA_SYSTEMCTL — systemctl binary (used by install.sh --diff, set in conf.sh)
# SPIRA_LOGINCTL  — loginctl binary for linger checks
# SPIRA_TMUX      — tmux binary for cockpit-pane checks
# SPIRA_INSTALL_FORCE=1 — passed through to install.sh --diff to bypass gate checks
#
# covers: systemd/units.sh spira/conf.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"

# Parse subcommand and optional instance BEFORE sourcing conf.sh so that SPIRA_INSTANCE
# is in the environment when conf.sh derives SPIRA_RUN, SPIRA_DB, and other
# instance-qualified paths from it.
_owned_mode="${1:-list}"
_owned_inst="${2:-}"
[ -n "$_owned_inst" ] && export SPIRA_INSTANCE="$_owned_inst"

. "$HERE/conf.sh"

# Test seams. conf.sh already exports SPIRA_SYSTEMCTL.
SPIRA_LOGINCTL="${SPIRA_LOGINCTL:-loginctl}"
SPIRA_TMUX="${SPIRA_TMUX:-tmux}"

UNITDIR="${HOME}/.config/systemd/user"
_owned_user="${USER:-$(id -un 2>/dev/null || true)}"

# SOURCE UNITS.SH from the same directory as the real install.sh. install.sh uses
# readlink -f to locate units.sh from the real file's directory — not the invocation
# path — so a fixture symlink at $FIXTURE/systemd/install.sh still resolves the real
# units.sh. We replicate that logic here so the two share exactly one definition of
# the unit list (law-prefer-the-real-dependency).
_owned_real_install="$(readlink -f "$HERE/../systemd/install.sh" 2>/dev/null \
    || printf '%s' "$HERE/../systemd/install.sh")"
. "$(dirname "$_owned_real_install")/units.sh" || exit 1
unset _owned_real_install

# ---------------------------------------------------------------------------
# LIST: kind|id|location|phase|retention
# ---------------------------------------------------------------------------
_row() { printf '%s|%s|%s|%s|%s\n' "$1" "$2" "$3" "$4" "$5"; }

_owned_units() {
    local u inst_u wname inst_w
    for u in "${UNITS[@]}"; do
        [ "$u" = "spira-watch@.service" ] && continue
        inst_u="$(inst_name "$u")"
        _row unit "$inst_u" "$UNITDIR/$inst_u" install keep
    done
    for wname in "${_watch_names[@]}"; do
        inst_w="$(inst_watch_name "$wname")"
        _row unit "$inst_w" "$UNITDIR/$inst_w" install keep
    done
    # UNITS THIS TREE WOULD NOT INSTALL, BUT ANOTHER TREE DID. units.sh leaves out a unit whose
    # program is not built HERE (UNBUILT, _watch_unbuilt), and the tree an install ran from is
    # not always the tree an uninstall runs from: a release tarball ships bin/, a source
    # checkout does not. Listed as optional so an absent one is simply absent, and so that an
    # uninstall from the checkout still removes the broker, loom and watcher units a tarball
    # install put there — the acceptance run left exactly those four behind.
    for u in "${UNBUILT[@]+"${UNBUILT[@]}"}"; do
        inst_u="$(inst_name "$u")"
        _row unit "$inst_u" "$UNITDIR/$inst_u" install optional
    done
    for wname in "${_watch_unbuilt[@]+"${_watch_unbuilt[@]}"}"; do
        inst_w="$(inst_watch_name "$wname")"
        _row unit "$inst_w" "$UNITDIR/$inst_w" install optional
    done
}

_owned_linger() {
    _row linger "$_owned_user" "loginctl:linger:$_owned_user" install keep
}

_owned_runtime() {
    _row runtime-tree spira-run "$SPIRA_RUN" install optional
}

_owned_database() {
    _row database spira-db "$SPIRA_DB" init keep
}

_owned_session_hooks() {
    local settings="${SPIRA_CLIENT_SETTINGS:-$HOME/.claude/settings.json}"
    _row session-hook SessionStart "$settings" install keep
    _row session-hook PostCompact  "$settings" install keep
}

_owned_alert_dropins() {
    [ -n "${SPIRA_ALERT_GLOB:-}" ] || return 0
    local f unit
    while IFS= read -r f; do
        unit="$(basename "$f")"
        _row alert-dropin "$unit" "$UNITDIR/$unit.d/50-spira-intake.conf" install optional
    done < <(find "$UNITDIR" -maxdepth 1 -name "$SPIRA_ALERT_GLOB" 2>/dev/null | sort)
}

_owned_binaries() {
    [ -n "${SPIRA_LOOM_BIN:-}" ] && _row binary spira-loom  "$SPIRA_LOOM_BIN" build keep
    [ -n "${SPIRA_PANEL:-}" ]    && _row binary spira-panel "$SPIRA_PANEL"     build keep
}

_owned_dolt_yaml() {
    [ -n "${SPIRA_DOLT_DATA:-}" ] && \
        _row dolt-yaml dolt-server      "$SPIRA_DOLT_DATA/dolt-server.yaml"   install optional
    [ -n "${SPIRA_TESTDB_DATA:-}" ] && \
        _row dolt-yaml dolt-server-test "$SPIRA_TESTDB_DATA/dolt-server.yaml" install optional
}

_owned_cockpit_panes() {
    _row cockpit-pane panel  "tmux:@cockpit=panel"  runtime optional
    _row cockpit-pane health "tmux:@cockpit=health" runtime optional
}

_owned_all() {
    _owned_units
    _owned_linger
    _owned_runtime
    _owned_database
    _owned_session_hooks
    _owned_alert_dropins
    _owned_binaries
    _owned_dolt_yaml
    _owned_cockpit_panes
}

if [ "$_owned_mode" = "list" ]; then
    _owned_all
    exit 0
fi

# ---------------------------------------------------------------------------
# CHECK: kind|id|location|status (present | absent | drifted)
# ---------------------------------------------------------------------------

# Run install.sh --diff once and cache the output. The diff covers all unit kinds
# simultaneously: MISSING  <name> → absent, DIFFERS  <name> → drifted, no mention → present.
_diff_out=""
_diff_noinstaller=0

_check_unit_diff() {
    local installer
    installer="$(readlink -f "$HERE/../systemd/install.sh" 2>/dev/null \
        || printf '%s' "$HERE/../systemd/install.sh")"
    if [ ! -f "$installer" ]; then
        _diff_noinstaller=1
        return 0
    fi
    # SPIRA_INSTALL_FORCE is inherited from the environment when set (tests pass it);
    # --diff exits before the landref and live-aeon checks regardless, so the guard
    # exists only to let a SPIRA_INSTALL_FORCE=1 test call this path without friction.
    _diff_out="$(bash "$installer" "$SPIRA_INSTANCE" --diff 2>/dev/null)" || true
}

_unit_status() {
    local name="$1"
    if [ "$_diff_noinstaller" = 1 ]; then
        printf 'absent'; return
    fi
    # install.sh --diff: MISSING  <name> (not installed), DIFFERS  <name>
    # A unit not mentioned at all passed the diff comparison and is present.
    if printf '%s\n' "$_diff_out" | grep -qE "^MISSING[[:space:]]+${name}([[:space:]]|$)"; then
        printf 'absent'
    elif printf '%s\n' "$_diff_out" | grep -qE "^DIFFERS[[:space:]]+${name}([[:space:]]|$)"; then
        printf 'drifted'
    else
        printf 'present'
    fi
}

_linger_status() {
    local out
    out="$("$SPIRA_LOGINCTL" show-user "$_owned_user" -p Linger 2>/dev/null || true)"
    [ "$out" = "Linger=yes" ] && printf 'present' || printf 'absent'
}

_file_status() {
    [ -e "$1" ] && printf 'present' || printf 'absent'
}

_session_hook_status() {
    local event="$1" out
    out="$("$HERE/install-session-hook.sh" status 2>/dev/null)" || true
    if printf '%s\n' "$out" | grep -qE "^ok[[:space:]]+$event([[:space:]]|$)"; then
        printf 'present'
    else
        printf 'absent'
    fi
}

_cockpit_pane_status() {
    local tag="$1" panes
    panes="$("$SPIRA_TMUX" list-panes -a -F '#{pane_id} #{@cockpit}' 2>/dev/null)" || {
        printf 'absent'; return
    }
    if printf '%s\n' "$panes" | grep -qE "[[:space:]]${tag}$"; then
        printf 'present'
    else
        printf 'absent'
    fi
}

_check_units() {
    local u inst_u status wname inst_w
    for u in "${UNITS[@]}"; do
        [ "$u" = "spira-watch@.service" ] && continue
        inst_u="$(inst_name "$u")"
        status="$(_unit_status "$inst_u")"
        printf '%s|%s|%s|%s\n' unit "$inst_u" "$UNITDIR/$inst_u" "$status"
    done
    for wname in "${_watch_names[@]}"; do
        inst_w="$(inst_watch_name "$wname")"
        status="$(_unit_status "$inst_w")"
        printf '%s|%s|%s|%s\n' unit "$inst_w" "$UNITDIR/$inst_w" "$status"
    done
}

_check_linger() {
    printf '%s|%s|%s|%s\n' \
        linger "$_owned_user" "loginctl:linger:$_owned_user" "$(_linger_status)"
}

_check_runtime() {
    printf '%s|%s|%s|%s\n' \
        runtime-tree spira-run "$SPIRA_RUN" "$(_file_status "$SPIRA_RUN")"
}

_check_database() {
    printf '%s|%s|%s|%s\n' \
        database spira-db "$SPIRA_DB" "$(_file_status "$SPIRA_DB")"
}

_check_session_hooks() {
    local settings="${SPIRA_CLIENT_SETTINGS:-$HOME/.claude/settings.json}"
    local event status
    for event in SessionStart PostCompact; do
        status="$(_session_hook_status "$event")"
        printf '%s|%s|%s|%s\n' session-hook "$event" "$settings" "$status"
    done
}

_check_alert_dropins() {
    [ -n "${SPIRA_ALERT_GLOB:-}" ] || return 0
    local f unit dropin status
    while IFS= read -r f; do
        unit="$(basename "$f")"
        dropin="$UNITDIR/$unit.d/50-spira-intake.conf"
        status="$(_file_status "$dropin")"
        printf '%s|%s|%s|%s\n' alert-dropin "$unit" "$dropin" "$status"
    done < <(find "$UNITDIR" -maxdepth 1 -name "$SPIRA_ALERT_GLOB" 2>/dev/null | sort)
}

_check_binaries() {
    local status
    if [ -n "${SPIRA_LOOM_BIN:-}" ]; then
        status="$(_file_status "$SPIRA_LOOM_BIN")"
        printf '%s|%s|%s|%s\n' binary spira-loom "$SPIRA_LOOM_BIN" "$status"
    fi
    if [ -n "${SPIRA_PANEL:-}" ]; then
        status="$(_file_status "$SPIRA_PANEL")"
        printf '%s|%s|%s|%s\n' binary spira-panel "$SPIRA_PANEL" "$status"
    fi
}

_check_dolt_yaml() {
    local status
    if [ -n "${SPIRA_DOLT_DATA:-}" ]; then
        status="$(_file_status "$SPIRA_DOLT_DATA/dolt-server.yaml")"
        printf '%s|%s|%s|%s\n' \
            dolt-yaml dolt-server "$SPIRA_DOLT_DATA/dolt-server.yaml" "$status"
    fi
    if [ -n "${SPIRA_TESTDB_DATA:-}" ]; then
        status="$(_file_status "$SPIRA_TESTDB_DATA/dolt-server.yaml")"
        printf '%s|%s|%s|%s\n' \
            dolt-yaml dolt-server-test "$SPIRA_TESTDB_DATA/dolt-server.yaml" "$status"
    fi
}

_check_cockpit_panes() {
    local tag status
    for tag in panel health; do
        status="$(_cockpit_pane_status "$tag")"
        printf '%s|%s|%s|%s\n' cockpit-pane "$tag" "tmux:@cockpit=$tag" "$status"
    done
}

_check_unit_diff

_check_units
_check_linger
_check_runtime
_check_database
_check_session_hooks
_check_alert_dropins
_check_binaries
_check_dolt_yaml
_check_cockpit_panes
