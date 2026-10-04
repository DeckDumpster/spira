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
#   dolt-yaml     dolt-server.yaml configs in SPIRA_DOLT_DATA / SPIRA_TESTDB_DATA
#   cockpit-pane  tmux panes tagged @cockpit=panel and @cockpit=health
#   alert-dropin  50-spira-intake.conf drop-ins for alert units
#
# TEST SEAMS
# ----------
# SPIRA_SYSTEMCTL — systemctl binary (used by units-install --diff, set in conf.sh)
# SPIRA_LOGINCTL  — loginctl binary for linger checks
# SPIRA_TMUX      — tmux binary for cockpit-pane checks
# SPIRA_INSTALL_FORCE=1 — passed through to units-install --diff to bypass gate checks
#
# covers: install/src/manifest.rs spira/conf.sh
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

# THE UNIT MANIFEST, from units-install itself (sp-31dm0: systemd/units.sh is retired; the
# manifest — UNITS/ENABLE/OPTIONAL/UNBUILT, inst_name, inst_watch_name — is install/src/
# manifest.rs now). `--list-manifest` prints the installed names already resolved for
# $SPIRA_INSTANCE, so this reads them directly rather than re-deriving inst_name/
# inst_watch_name in bash a second time (law-prefer-the-real-dependency).
declare -a _OWNED_UNIT_NAMES=() _OWNED_UNBUILT_NAMES=()
while IFS=' ' read -r _kind _name; do
    case "$_kind" in
        unit)    _OWNED_UNIT_NAMES+=("$_name") ;;
        unbuilt) _OWNED_UNBUILT_NAMES+=("$_name") ;;
    esac
done < <(units-install --list-manifest) || exit 1

# UNION IN WHAT IS ACTUALLY ON DISK FOR THIS INSTANCE (sp-da4y0). --list-manifest reflects
# this box's CURRENT resolution of conditional inputs (e.g. Inputs::repo_is_git_checkout,
# probed fresh by manifest_from_env on every call, never frozen at install time) — a unit
# installed under yesterday's resolution that today's resolves differently (the repo was a
# git checkout then, is not seen as one now, or vice versa) is invisible to --list-manifest,
# so a removal pass driven by that list alone leaves it behind. Acceptance hit this exactly
# (run 37222619835): install resolved the repo as a git checkout and installed
# spira-cert-sweep-{full,sample}-prod.{service,timer}; uninstall's own re-resolution saw it
# as not one, so those four were absent from the manifest and the stray sweep only reported
# them, never removed them. A real spira-*-<instance> unit FILE already on disk is ground
# truth regardless of what a fresh recomputation says now, so fold in every one found —
# matching how the manifest's own `unbuilt` list already keeps an earlier run's
# dependency-missing units removable. Safe from transients: spira-aeon-*/spira-landing/
# spira-audit* are systemd-run and never file-backed, so this glob cannot see one; a
# different instance's files never match the -<instance> suffix.
_owned_glob_inst="${SPIRA_INSTANCE:-prod}"
if [ -n "$_owned_glob_inst" ]; then
    _owned_known=" "
    for _og_n in "${_OWNED_UNIT_NAMES[@]+"${_OWNED_UNIT_NAMES[@]}"}"; do
        _owned_known="$_owned_known$_og_n "
    done
    for _og_f in "$UNITDIR"/spira-*-"$_owned_glob_inst".service "$UNITDIR"/spira-*-"$_owned_glob_inst".timer; do
        [ -e "$_og_f" ] || continue
        _og_bn="$(basename "$_og_f")"
        case "$_owned_known" in
            *" $_og_bn "*) ;;
            *) _OWNED_UNIT_NAMES+=("$_og_bn"); _owned_known="$_owned_known$_og_bn " ;;
        esac
    done
    unset _og_n _og_f _og_bn _owned_known
fi
unset _owned_glob_inst

# ---------------------------------------------------------------------------
# LIST: kind|id|location|phase|retention
# ---------------------------------------------------------------------------
_row() { printf '%s|%s|%s|%s|%s\n' "$1" "$2" "$3" "$4" "$5"; }

_owned_units() {
    local inst_u
    for inst_u in "${_OWNED_UNIT_NAMES[@]+"${_OWNED_UNIT_NAMES[@]}"}"; do
        _row unit "$inst_u" "$UNITDIR/$inst_u" install keep
    done
    # UNITS THIS BOX DECLINED (UNBUILT: a unit whose external program — inotifywait — is
    # absent here). Listed as optional so an absent one is simply absent, and so an uninstall
    # still removes one another install put there.
    for inst_u in "${_OWNED_UNBUILT_NAMES[@]+"${_OWNED_UNBUILT_NAMES[@]}"}"; do
        _row unit "$inst_u" "$UNITDIR/$inst_u" install optional
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
    # Binaries are the release's (sp-gypjk): nothing an install owns lives outside it.
    :
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
    out="$(release session-hook status 2>/dev/null)" || true
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
    local inst_u status
    for inst_u in "${_OWNED_UNIT_NAMES[@]+"${_OWNED_UNIT_NAMES[@]}"}"; do
        status="$(_unit_status "$inst_u")"
        printf '%s|%s|%s|%s\n' unit "$inst_u" "$UNITDIR/$inst_u" "$status"
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
    :   # binaries are the release's (sp-gypjk) — no install-owned binary rows
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
