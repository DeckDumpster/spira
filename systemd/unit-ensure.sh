#!/usr/bin/env bash
# unit-ensure.sh — render and install unit templates that are missing from or differ
# from the installed copies, without triggering the live-aeons fence.
#
# Safe while aeons are active: installs new/changed files and daemon-reloads, but
# does not restart running services. Idempotent: a second run is a no-op when all
# units are current.
#
#   unit-ensure.sh         install any missing or changed units
#   unit-ensure.sh --diff  report without changing anything (like install.sh --diff)
set -uo pipefail

SRC="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
_real="$(dirname "$(readlink -f "${BASH_SOURCE[0]}" 2>/dev/null || printf '%s' "${BASH_SOURCE[0]}")")"
. "$(cd "$SRC/../spira" && pwd -P)/conf.sh"

_ue_diff=0
for _a in "$@"; do
    case "$_a" in --diff) _ue_diff=1 ;; esac
done

if [ "$_ue_diff" -eq 1 ]; then
    exec bash "$_real/install.sh" --diff
fi
unset _a

DEST="$HOME/.config/systemd/user"
mkdir -p "$DEST"

. "$_real/units.sh" || exit 1

SC="${SPIRA_SYSTEMCTL:-systemctl}"
DOLT="$(command -v dolt 2>/dev/null || true)"

# render <template> [<watcher-name>] — the same render.py install.sh uses, so the
# two callers can never drift the way their independent heredoc copies once did.
render() {
    python3 "$_real/render.py" "$1" \
        --home "$SPIRA_HOME" --repo "$SPIRA_REPO" --run "$SPIRA_RUN" --db "$SPIRA_DB" \
        --cockpit "$SPIRA_COCKPIT" --dolt-data "$SPIRA_DOLT_DATA" \
        --testdb-data "$SPIRA_TESTDB_DATA" --dolt "$DOLT" --prod "$SPIRA_PROD" \
        --instance "$SPIRA_INSTANCE" --testdb-port "$SPIRA_TESTDB_PORT" \
        --supervise-bin "$SPIRA_SUPERVISE_BIN" --snap-stale-s "$SPIRA_SNAP_STALE_S" \
        --landing-pass-bin "$SPIRA_LANDING_PASS_BIN" \
        --reconciler-flow-bin "$SPIRA_RECONCILER_FLOW_BIN" --watcher-name "${2:-}"
}

declare -A _ue_new=()
n_changed=0
n_unchanged=0

_ue_write() {  # _ue_write <inst> <text>
    local inst="$1" text="$2"
    printf '%s\n' "$text" > "$DEST/$inst.new"
    mv "$DEST/$inst.new" "$DEST/$inst" && chmod 0644 "$DEST/$inst"
}

for u in "${UNITS[@]}"; do
    [ "$u" = "spira-watch@.service" ] && continue
    inst="$(inst_name "$u")"
    text="$(render "$SRC/$u")" || { printf 'unit-ensure: render failed for %s\n' "$u" >&2; continue; }
    # Skip masked units (symlink to /dev/null).
    [ -L "$DEST/$inst" ] && [ "$(readlink "$DEST/$inst" 2>/dev/null)" = "/dev/null" ] && continue
    if [ ! -f "$DEST/$inst" ]; then
        _ue_write "$inst" "$text"
        printf 'unit-ensure: installed  %s\n' "$inst"
        _ue_new[$inst]=1; n_changed=$((n_changed+1))
    elif ! cmp -s <(printf '%s\n' "$text") "$DEST/$inst"; then
        _ue_write "$inst" "$text"
        printf 'unit-ensure: updated    %s\n' "$inst"
        n_changed=$((n_changed+1))
    else
        n_unchanged=$((n_unchanged+1))
    fi
done

for _wname in "${_watch_names[@]}"; do
    inst="$(inst_watch_name "$_wname")"
    text="$(render "$SRC/spira-watch@.service" "$_wname")" \
        || { printf 'unit-ensure: render failed for watcher %s\n' "$_wname" >&2; continue; }
    [ -L "$DEST/$inst" ] && [ "$(readlink "$DEST/$inst" 2>/dev/null)" = "/dev/null" ] && continue
    if [ ! -f "$DEST/$inst" ]; then
        _ue_write "$inst" "$text"
        printf 'unit-ensure: installed  %s\n' "$inst"
        _ue_new[$inst]=1; n_changed=$((n_changed+1))
    elif ! cmp -s <(printf '%s\n' "$text") "$DEST/$inst"; then
        _ue_write "$inst" "$text"
        printf 'unit-ensure: updated    %s\n' "$inst"
        n_changed=$((n_changed+1))
    else
        n_unchanged=$((n_unchanged+1))
    fi
done

# The guards below (BINARY GUARD, PRODUCER GUARD) must run every invocation, not only
# when a unit file's rendered content changed — an enabled-but-should-be-disabled unit
# stays that way forever otherwise, since nothing about its own file content depends
# on the binary or producer state that makes it wrong to run.
if [ "$n_changed" -gt 0 ]; then
    "$SC" --user daemon-reload
    printf 'unit-ensure: daemon-reload after %d change(s), %d unchanged\n' "$n_changed" "$n_unchanged"
else
    printf 'unit-ensure: no changes — %d unit(s) current\n' "$n_unchanged"
fi

# Enable and start newly installed units that belong to the ENABLE set.
# Updated (DIFFERS) units are not restarted — that is the operator's call.
#
# MISSING-TARGET GUARD. A unit whose ExecStart points at a binary that does not
# exist would exit 127 on every tick — worse than an absent unit, because
# systemctl reports it as enabled. Parse the ExecStart line from the rendered
# file and refuse to enable if the target is not executable.
_ue_execstart_ok() {  # _ue_execstart_ok <unit-file>
    local f="$1"
    # ExecStart= may have args; the executable is the first token after the '='.
    local line exec_bin
    line="$(grep -m1 '^ExecStart=' "$f" 2>/dev/null || true)"
    [ -n "$line" ] || return 0   # no ExecStart — let systemd decide
    exec_bin="${line#ExecStart=}"
    exec_bin="${exec_bin%% *}"   # first token only
    [ -n "$exec_bin" ] || return 1
    [ -x "$exec_bin" ]
}

for _en in "${ENABLE[@]}"; do
    [ -n "${_ue_new[$_en]:-}" ] || continue
    _ue_file="$DEST/$_en"
    if [ -f "$_ue_file" ] && ! _ue_execstart_ok "$_ue_file"; then
        _ue_exec="$(grep -m1 '^ExecStart=' "$_ue_file" | sed 's/^ExecStart=//;s/ .*//')"
        printf 'unit-ensure: MISSING-TARGET  %s (ExecStart target not executable: %s)\n' \
            "$_en" "${_ue_exec:-<empty>}" >&2
        continue
    fi
    "$SC" --user enable "$_en" >/dev/null 2>&1 \
        && "$SC" --user start  "$_en" >/dev/null 2>&1 \
        && printf 'unit-ensure: enabled+started  %s\n' "$_en" \
        || printf 'unit-ensure: failed to enable %s\n' "$_en" >&2
done
unset _ue_file _ue_exec

# BINARY GUARD. A unit whose binary is not executable will exit 127 on every tick whether
# or not cargo is on PATH — "cargo present but build never ran" is the case the old guard
# missed by gating the whole loop on cargo being absent. Disable any such enabled unit;
# doctor reports the missing binary and names the correct remedy.
for _ue_pair in \
    "${SPIRA_LOOM_BIN:-}:loom:service" \
    "${SPIRA_BROKER_BIN:-}:broker:timer" \
    "${SPIRA_PANEL:-}:cockpit:service" \
    "${SPIRA_CZAR_PASS_BIN:-}:czar-pass:timer"; do
    _ue_cbin="${_ue_pair%%:*}"
    _ue_rest="${_ue_pair#*:}"
    _ue_cbase="${_ue_rest%%:*}"
    _ue_ctype="${_ue_rest#*:}"
    [ -n "${_ue_cbin:-}" ] || continue
    [ -x "${_ue_cbin}" ] && continue
    _ue_cargo_note=""
    command -v cargo >/dev/null 2>&1 || _ue_cargo_note=" (cargo not on PATH)"
    for _ue_cname in \
        "spira-${_ue_cbase}${SPIRA_INSTANCE:+-$SPIRA_INSTANCE}.${_ue_ctype}" \
        "spira-${_ue_cbase}.${_ue_ctype}"; do
        "$SC" --user is-enabled "$_ue_cname" >/dev/null 2>&1 || continue
        "$SC" --user disable "$_ue_cname" >/dev/null 2>&1 \
            && printf 'unit-ensure: DISABLED %s (binary not executable at %s%s)\n' \
                "$_ue_cname" "${_ue_cbin}" "${_ue_cargo_note}" \
            || printf 'unit-ensure: WARNING could not disable %s\n' "$_ue_cname" >&2
    done
done
unset _ue_pair _ue_cbin _ue_rest _ue_cbase _ue_ctype _ue_cname _ue_cargo_note

# PRODUCER GUARD. spira-broker.timer's enabled state must track spira_broker_producer_present
# on every invocation, not only when the unit is newly installed or its rendered content
# changes — the ENABLE loop above only reaches new units, but the timer's own file never
# changes when the operator flips SPIRA_BROKER_ENABLE, so that loop would never turn it on.
# Reads the same predicate units.sh gates ENABLE on (units.sh sourced above). The producer
# decision is made once, not per candidate name — a name with no installed file (the bare
# name when SPIRA_INSTANCE is set, or vice versa) must not fall through to disable just
# because its own file is missing while the producer is in fact present.
if spira_broker_producer_present && [ -x "${SPIRA_BROKER_BIN:-}" ]; then
    for _ue_bname in \
        "spira-broker${SPIRA_INSTANCE:+-$SPIRA_INSTANCE}.timer" \
        "spira-broker.timer"; do
        _ue_bfile="$DEST/$_ue_bname"
        [ -f "$_ue_bfile" ] && _ue_execstart_ok "$_ue_bfile" || continue
        "$SC" --user enable "$_ue_bname" >/dev/null 2>&1 \
            && "$SC" --user start "$_ue_bname" >/dev/null 2>&1 \
            && printf 'unit-ensure: enabled+started  %s\n' "$_ue_bname" \
            || printf 'unit-ensure: failed to enable %s\n' "$_ue_bname" >&2
    done
else
    for _ue_bname in \
        "spira-broker${SPIRA_INSTANCE:+-$SPIRA_INSTANCE}.timer" \
        "spira-broker.timer"; do
        "$SC" --user is-enabled "$_ue_bname" >/dev/null 2>&1 || continue
        "$SC" --user disable "$_ue_bname" >/dev/null 2>&1 \
            && printf 'unit-ensure: DISABLED %s (no producer; SPIRA_BROKER_ENABLE=1 to opt in)\n' "$_ue_bname" \
            || printf 'unit-ensure: WARNING could not disable %s\n' "$_ue_bname" >&2
    done
fi
unset _ue_bname _ue_bfile
