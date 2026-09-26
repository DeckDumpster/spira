#!/usr/bin/env bash
# uninstall.sh — inverse of install.sh; walks owned.sh; data-preserving by default.
#
#   uninstall.sh [<instance>] [--yes] [--dry-run] [--purge] [--purge-database]
#
# THREE RETENTION TIERS
# ---------------------
# Removed by default:
#   systemd units (stop, disable, delete files, daemon-reload)
#   linger flag (installer sets it; owned.sh declares it)
#   ~/.local/bin symlinks pointing into this harness tree
#   session hooks in the agent settings file
#   alert drop-ins (install-intake.sh uninstall)
#   cockpit panes (layout.sh down — sessions are never killed)
#
# Kept unless --purge:
#   ~/.config/spira/  (the config the operator wrote)
#   $SPIRA_RUN        (runtime tree; holds the transcript archive)
#
# Kept unless --purge-database (requires bead count typed back):
#   $SPIRA_DB              (beads database and its Dolt data)
#   $SPIRA_DOLT_DATA       (Dolt server data directory)
#   $SPIRA_TESTDB_DATA     (test Dolt server data directory)
#
# INSTANCE AWARENESS
# ------------------
# With an instance argument: removes only that instance's units.
# With no argument and exactly one instance installed: removes that instance.
# With no argument and multiple instances: refuses to guess.
#
# PARTIAL INSTALL SAFETY
# ----------------------
# Tolerates every artifact being absent. Does not exit non-zero merely because
# there was less to remove than expected. A second run exits 0.
#
# POSITIVE CONTROL
# ----------------
# After removing the inventory, sweeps for spira-* units and the four shared
# names (cockpit-ensure, concierge, beads-push, dolt-beads). Reports anything
# found that the owned.sh manifest did not predict — these are "stray" artifacts
# from an older harness version or a different install that the manifest-driven
# pass misses by design.
#
# TEST SEAMS
# ----------
# SPIRA_SYSTEMCTL  — systemctl binary (set by conf.sh)
# SPIRA_LOGINCTL   — loginctl binary for linger management
# SPIRA_TMUX       — tmux binary for cockpit pane removal
# SPIRA_BD         — bd binary for the --purge-database bead count
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"

# ---------------------------------------------------------------------------
# PARSE ARGUMENTS before sourcing conf.sh so SPIRA_INSTANCE is in the
# environment when conf.sh derives instance-qualified paths.
# ---------------------------------------------------------------------------
_un_instance=""
_un_yes=0
_un_dry=0
_un_purge=0
_un_purge_db=0
_un_exit=0
for _a in "$@"; do
    case "$_a" in
        --yes)             _un_yes=1 ;;
        --dry-run)         _un_dry=1 ;;
        --purge)           _un_purge=1 ;;
        --purge-database)  _un_purge_db=1 ;;
        --*)               printf 'uninstall: unknown flag: %s\n' "$_a" >&2; exit 2 ;;
        *)  [ -z "$_un_instance" ] && _un_instance="$_a" \
            || { printf 'uninstall: extra argument: %s\n' "$_a" >&2; exit 2; }
            ;;
    esac
done
unset _a
[ -n "$_un_instance" ] && export SPIRA_INSTANCE="$_un_instance"
unset _un_instance

. "$HERE/conf.sh"

SPIRA_LOGINCTL="${SPIRA_LOGINCTL:-loginctl}"
SPIRA_TMUX="${SPIRA_TMUX:-tmux}"
UNITDIR="${HOME}/.config/systemd/user"
_un_user="${USER:-$(id -un 2>/dev/null || true)}"

# ---------------------------------------------------------------------------
# INSTANCE AUTO-DETECTION. When no instance was given, scan UNITDIR for
# installed spira-sentinel-*.service files and extract the instance names from
# them. Exactly one → use it. Zero or many → refuse rather than guess.
# ---------------------------------------------------------------------------
if [ -z "${SPIRA_INSTANCE:-}" ] || [ "${SPIRA_INSTANCE}" = "prod" ]; then
    # Re-scan in case the default 'prod' was set because nothing was provided.
    _un_instances=()
    for _f in "$UNITDIR"/spira-sentinel-*.service; do
        [ -e "$_f" ] || continue
        _b="$(basename "$_f")"
        # spira-sentinel-<instance>.service — extract instance after last '-'
        _inst="${_b%.service}"; _inst="${_inst##*-}"
        _un_instances+=("$_inst")
    done
    unset _f _b _inst
    _un_cnt="${#_un_instances[@]}"
    if [ "$_un_cnt" -eq 0 ]; then
        : # No instances found — proceed with whatever conf.sh resolved; owned.sh will report absent
    elif [ "$_un_cnt" -eq 1 ]; then
        export SPIRA_INSTANCE="${_un_instances[0]}"
        # Re-source conf.sh with the detected instance so paths are correct.
        unset _SPIRA_UNITS_LOADED 2>/dev/null || true
        . "$HERE/conf.sh"
    else
        printf 'uninstall: multiple Spira instances installed: %s\n' "${_un_instances[*]}" >&2
        printf 'uninstall: re-run with the instance name to uninstall, e.g.:\n' >&2
        for _i in "${_un_instances[@]}"; do
            printf '    uninstall.sh %s\n' "$_i" >&2
        done
        unset _i
        exit 1
    fi
    unset _un_cnt _un_instances
fi

# ---------------------------------------------------------------------------
# COLLECT THE MANIFEST from owned.sh. Parse: kind|id|location|phase|retention
# ---------------------------------------------------------------------------
_un_manifest="$("$HERE/owned.sh" list "$SPIRA_INSTANCE" 2>/dev/null)" || {
    printf 'uninstall: owned.sh list failed\n' >&2; exit 1; }

# Build parallel arrays for grouped display.
_un_unit_names=()
_un_unit_paths=()
_un_linger_user=""
_un_session_settings=""
_un_dropin_paths=()
_un_dropin_ids=()
_un_cockpit_panes=()

while IFS='|' read -r kind id loc phase retention; do
    [ -n "$kind" ] || continue
    case "$kind" in
        unit)         _un_unit_names+=("$id"); _un_unit_paths+=("$loc") ;;
        linger)       _un_linger_user="$id" ;;
        session-hook) [ -z "$_un_session_settings" ] && _un_session_settings="$loc" ;;
        alert-dropin) _un_dropin_ids+=("$id"); _un_dropin_paths+=("$loc") ;;
        cockpit-pane) _un_cockpit_panes+=("$id") ;;
    esac
done <<< "$_un_manifest"

# Discover ~/.local/bin symlinks that point into this harness tree.
_un_harness_parent="$(dirname "$SPIRA_HOME")"
_un_bin_links=()
for _lname in cockpit cockpit-remote; do
    _lpath="$HOME/.local/bin/$_lname"
    if [ -L "$_lpath" ]; then
        _ltgt="$(readlink "$_lpath" 2>/dev/null || true)"
        case "$_ltgt" in "$_un_harness_parent"/*) _un_bin_links+=("$_lpath") ;; esac
    fi
done
unset _lname _lpath _ltgt

# ---------------------------------------------------------------------------
# PREVIEW. Print what will be removed, grouped by kind.
# ---------------------------------------------------------------------------
_un_lines() {
    local label="$1"; shift
    [ "${#@}" -eq 0 ] && return
    printf '\n  %s:\n' "$label"
    for _x in "$@"; do printf '    %s\n' "$_x"; done
}

if [ "$_un_dry" = 1 ]; then
    printf 'uninstall: DRY RUN — nothing will be changed.\n\n'
fi

printf 'Spira instance: %s\n' "${SPIRA_INSTANCE:-prod}"

_un_lines "units (stop, disable, remove)" "${_un_unit_names[@]+"${_un_unit_names[@]}"}"
[ -n "$_un_linger_user" ] && printf '\n  linger: %s\n' "$_un_linger_user"
_un_lines "~/.local/bin symlinks" "${_un_bin_links[@]+"${_un_bin_links[@]}"}"
[ -n "$_un_session_settings" ] && printf '\n  session hooks: %s\n' "$_un_session_settings"
_un_lines "alert drop-ins" "${_un_dropin_ids[@]+"${_un_dropin_ids[@]}"}"
_un_lines "cockpit panes" "${_un_cockpit_panes[@]+"${_un_cockpit_panes[@]}"}"

if [ "$_un_purge" = 1 ]; then
    printf '\n  --purge: config dir:  %s\n' "${XDG_CONFIG_HOME:-$HOME/.config}/spira"
    printf '  --purge: runtime dir: %s\n' "$SPIRA_RUN"
else
    printf '\n  (kept — re-run with --purge to also remove config and runtime)\n'
fi

if [ "$_un_purge_db" = 1 ]; then
    printf '  --purge-database: beads database: %s\n' "$SPIRA_DB"
    [ -n "${SPIRA_DOLT_DATA:-}" ]   && printf '  --purge-database: Dolt data:       %s\n' "$SPIRA_DOLT_DATA"
    [ -n "${SPIRA_TESTDB_DATA:-}" ] && printf '  --purge-database: test Dolt data:  %s\n' "$SPIRA_TESTDB_DATA"
else
    printf '  (kept — re-run with --purge-database to also remove the beads database)\n'
fi

printf '\n'

# ---------------------------------------------------------------------------
# CONFIRMATION. Skipped by --yes or --dry-run.
# ---------------------------------------------------------------------------
if [ "$_un_dry" = 0 ] && [ "$_un_yes" = 0 ]; then
    printf 'Proceed? [y/N] '
    read -r _un_reply
    case "${_un_reply:-}" in [yY]*) ;; *)
        printf 'uninstall: cancelled.\n'; exit 0 ;;
    esac
    unset _un_reply
fi

[ "$_un_dry" = 1 ] && { printf 'uninstall: dry run complete.\n'; exit 0; }

# ---------------------------------------------------------------------------
# HELPER: act <description> <command...> — run the command, tolerate failure.
# ---------------------------------------------------------------------------
_un_act() {
    local desc="$1"; shift
    printf '  %s\n' "$desc"
    "$@" 2>/dev/null || true
}

# ---------------------------------------------------------------------------
# 1. STOP AND DISABLE UNITS. Every .timer first, across the WHOLE set, before
#    any .service — a timer stopped only after its own service already had its
#    turn is a race: a firing in that gap restarts the service and nothing
#    stops it again (sp-6k3pr). Disable next; remove files last; reload after.
# ---------------------------------------------------------------------------
_un_stopped=0
_un_removed=0
_un_removed_names=()   # for sweep exclusion

_un_stop_disable() {
    local u="$1"
    "${SPIRA_SYSTEMCTL:-systemctl}" --user stop "$u" 2>/dev/null && \
        _un_stopped=$((_un_stopped+1)) || true
    "${SPIRA_SYSTEMCTL:-systemctl}" --user disable "$u" 2>/dev/null || true
    _un_removed_names+=("$u")
}

if [ "${#_un_unit_names[@]}" -gt 0 ]; then
    printf '\nStopping and disabling units...\n'
    for _un_u in "${_un_unit_names[@]}"; do
        case "$_un_u" in *.timer) _un_stop_disable "$_un_u" ;; esac
    done
    for _un_u in "${_un_unit_names[@]}"; do
        case "$_un_u" in *.timer) continue ;; esac
        _un_stop_disable "$_un_u"
    done
    printf '\nRemoving unit files...\n'
    for _un_p in "${_un_unit_paths[@]}"; do
        if [ -f "$_un_p" ]; then
            rm -f "$_un_p"
            printf '  removed %s\n' "$(basename "$_un_p")"
            _un_removed=$((_un_removed+1))
        fi
    done
    "${SPIRA_SYSTEMCTL:-systemctl}" --user daemon-reload 2>/dev/null || true
    printf 'units: stopped %d, removed %d files\n' "$_un_stopped" "$_un_removed"
fi

# ---------------------------------------------------------------------------
# 2. LINGER. Remove only if currently enabled.
# ---------------------------------------------------------------------------
if [ -n "$_un_linger_user" ]; then
    _un_cur_linger="$("$SPIRA_LOGINCTL" show-user "$_un_linger_user" -p Linger 2>/dev/null || true)"
    if [ "$_un_cur_linger" = "Linger=yes" ]; then
        _un_act "disabling linger for $_un_linger_user" \
            "$SPIRA_LOGINCTL" disable-linger "$_un_linger_user"
    else
        printf '  linger: already off for %s\n' "$_un_linger_user"
    fi
fi

# ---------------------------------------------------------------------------
# 3. ~/.local/bin SYMLINKS pointing into this harness.
# ---------------------------------------------------------------------------
if [ "${#_un_bin_links[@]}" -gt 0 ]; then
    printf '\nRemoving ~/.local/bin symlinks...\n'
    for _un_l in "${_un_bin_links[@]}"; do
        _un_act "removing $_un_l" rm -f "$_un_l"
    done
fi

# ---------------------------------------------------------------------------
# 4. SESSION HOOKS in the agent settings file.
# ---------------------------------------------------------------------------
if [ -n "$_un_session_settings" ] && [ -f "$HERE/install-session-hook.sh" ]; then
    printf '\nRemoving session hooks...\n'
    "$HERE/install-session-hook.sh" uninstall 2>/dev/null || true
fi

# ---------------------------------------------------------------------------
# 5. ALERT DROP-INS. Delegate to install-intake.sh which knows the exact shape.
# ---------------------------------------------------------------------------
if [ "${#_un_dropin_paths[@]}" -gt 0 ] && [ -f "$HERE/install-intake.sh" ]; then
    printf '\nRemoving alert drop-ins...\n'
    # install-intake.sh uninstall needs SPIRA_ALERT_GLOB to know which templates
    # to check. The dropin file names tell us exactly which ones exist; remove
    # each directly in case SPIRA_ALERT_GLOB is no longer set in the environment.
    for _un_dp in "${_un_dropin_paths[@]}"; do
        if [ -f "$_un_dp" ]; then
            rm -f "$_un_dp"
            rmdir "$(dirname "$_un_dp")" 2>/dev/null || true
            printf '  removed %s\n' "$_un_dp"
        fi
    done
    # Run daemon-reload once after all drop-ins are removed.
    "${SPIRA_SYSTEMCTL:-systemctl}" --user daemon-reload 2>/dev/null || true
fi

# ---------------------------------------------------------------------------
# 6. COCKPIT PANES. Run layout.sh down; it removes tagged panes only and
#    never kills the operator's session.
# ---------------------------------------------------------------------------
if [ "${#_un_cockpit_panes[@]}" -gt 0 ]; then
    _un_cockpit_layout="$(dirname "$SPIRA_HOME")/cockpit/layout.sh"
    if [ -x "$_un_cockpit_layout" ]; then
        printf '\nRemoving cockpit panes...\n'
        "$_un_cockpit_layout" down 2>/dev/null || true
    fi
fi

# ---------------------------------------------------------------------------
# 7. --purge: remove config directory and runtime tree.
# ---------------------------------------------------------------------------
if [ "$_un_purge" = 1 ]; then
    _un_conf_dir="${XDG_CONFIG_HOME:-$HOME/.config}/spira"
    printf '\n--purge: removing config and runtime...\n'
    if [ -d "$_un_conf_dir" ]; then
        _un_act "removing config dir: $_un_conf_dir" rm -rf "$_un_conf_dir"
    else
        printf '  config dir already absent: %s\n' "$_un_conf_dir"
    fi
    if [ -d "$SPIRA_RUN" ]; then
        _un_act "removing runtime dir: $SPIRA_RUN" rm -rf "$SPIRA_RUN"
    else
        printf '  runtime dir already absent: %s\n' "$SPIRA_RUN"
    fi
fi

# ---------------------------------------------------------------------------
# 8. --purge-database: count beads, require confirmation, then remove.
# ---------------------------------------------------------------------------
if [ "$_un_purge_db" = 1 ]; then
    printf '\n--purge-database: counting beads...\n'
    _un_bead_count=0
    if [ -d "${SPIRA_DB:-}/.beads" ] && command -v "${SPIRA_BD:-bd}" >/dev/null 2>&1; then
        _un_bead_count="$("${SPIRA_BD:-bd}" -C "$SPIRA_DB" list --all --format=json 2>/dev/null \
            | python3 -c 'import json,sys; print(len(json.load(sys.stdin)))' 2>/dev/null || echo 0)"
    fi
    printf 'The database at %s contains %s bead(s).\n' "$SPIRA_DB" "$_un_bead_count"
    printf 'Type the count to confirm deletion: '
    read -r _un_db_confirm
    if [ "${_un_db_confirm:-}" = "$_un_bead_count" ]; then
        [ -d "$SPIRA_DB" ]            && { _un_act "removing database: $SPIRA_DB" rm -rf "$SPIRA_DB"; }
        [ -n "${SPIRA_DOLT_DATA:-}" ] && [ -d "$SPIRA_DOLT_DATA" ] && \
            { _un_act "removing Dolt data: $SPIRA_DOLT_DATA" rm -rf "$SPIRA_DOLT_DATA"; }
        [ -n "${SPIRA_TESTDB_DATA:-}" ] && [ -d "$SPIRA_TESTDB_DATA" ] && \
            { _un_act "removing test Dolt data: $SPIRA_TESTDB_DATA" rm -rf "$SPIRA_TESTDB_DATA"; }
    else
        printf 'uninstall: count mismatch — database NOT removed.\n' >&2
        _un_exit=1
    fi
    unset _un_db_confirm _un_bead_count
fi

# ---------------------------------------------------------------------------
# 9. POSITIVE CONTROL SWEEP. After removing the inventory, look for anything
#    that matches spira-* or the four shared names but was NOT in the manifest.
#    A unit from a harness older than owned.sh is exactly what a manifest-driven
#    uninstaller misses; this sweep is the alarm that catches it.
# ---------------------------------------------------------------------------
printf '\nSweeping for unlisted Spira units...\n'
_FOUR_SHARED="cockpit-ensure concierge beads-push dolt-beads"

# Build a set of names the manifest predicted (already removed or never there).
_un_known=" "
for _un_n in "${_un_removed_names[@]+"${_un_removed_names[@]}"}"; do
    _un_known="$_un_known$_un_n "
done
# Add the four shared names even if they were absent from this instance's manifest —
# the sweep should not report them if this install simply did not include them.
for _f4 in $_FOUR_SHARED; do
    _un_known="$_un_known$_f4 "
    _un_known="$_un_known$_f4.service "
    _un_known="$_un_known$_f4.timer "
done
unset _un_n _f4

_un_strays=()

# Unit files still present in UNITDIR after the removal pass.
for _un_uf in "$UNITDIR"/spira-*.service "$UNITDIR"/spira-*.timer \
              "$UNITDIR"/cockpit-ensure.service "$UNITDIR"/cockpit-ensure.timer \
              "$UNITDIR"/concierge.service "$UNITDIR"/concierge.timer \
              "$UNITDIR"/beads-push.service "$UNITDIR"/beads-push.timer \
              "$UNITDIR"/dolt-beads.service "$UNITDIR"/dolt-beads.timer; do
    [ -e "$_un_uf" ] || continue
    _un_bn="$(basename "$_un_uf")"
    case "$_un_known" in *" $_un_bn "*) ;; *)
        _un_strays+=("$_un_bn (file: $_un_uf)") ;;
    esac
done
unset _un_uf _un_bn

# Units loaded in systemd but not backed by a file we just removed.
_un_loaded="$("${SPIRA_SYSTEMCTL:-systemctl}" --user list-units --all --no-legend \
    --plain 'spira-*' 'cockpit-ensure.*' 'concierge.*' 'beads-push.*' 'dolt-beads.*' 2>/dev/null \
    | awk '{print $1}' | sort -u || true)"
while IFS= read -r _un_ln; do
    [ -n "$_un_ln" ] || continue
    case "$_un_known" in *" $_un_ln "*) ;; *)
        _un_strays+=("$_un_ln (loaded in systemd)") ;;
    esac
done <<< "$_un_loaded"
unset _un_loaded _un_ln

if [ "${#_un_strays[@]}" -gt 0 ]; then
    printf 'STRAY UNITS FOUND (not in the owned manifest — may be from an older install):\n'
    for _un_s in "${_un_strays[@]}"; do
        printf '  STRAY  %s\n' "$_un_s"
    done
    printf 'These were not removed. Remove them by name if they belong to this installation.\n'
else
    printf '  sweep clean — no unlisted Spira units found\n'
fi
unset _un_strays _un_s

printf '\nuninstall: done.\n'
exit "$_un_exit"
