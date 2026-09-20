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
unset _real

SC="${SPIRA_SYSTEMCTL:-systemctl}"
DOLT="$(command -v dolt 2>/dev/null || true)"

# render <template> [<watcher-name>] — kept in sync with install.sh render().
render() {
    python3 - "$1" "$SPIRA_HOME" "$SPIRA_REPO" "$SPIRA_RUN" "$SPIRA_DB" "$SPIRA_COCKPIT" \
                   "$SPIRA_DOLT_DATA" "$SPIRA_TESTDB_DATA" "$DOLT" "$SPIRA_PROD" \
                   "$SPIRA_INSTANCE" "$SPIRA_TESTDB_PORT" "${2:-}" <<'PY'
import os, re, sys
keys = ["SPIRA_HOME", "SPIRA_REPO", "SPIRA_RUN", "SPIRA_DB", "SPIRA_COCKPIT",
        "SPIRA_DOLT_DATA", "SPIRA_TESTDB_DATA", "DOLT", "SPIRA_PROD", "SPIRA_INSTANCE",
        "SPIRA_TESTDB_PORT"]
m = dict(zip(keys, sys.argv[2:13]))
watcher_name = sys.argv[13] if len(sys.argv) > 13 else ""
if not m["SPIRA_PROD"]:
    m["SPIRA_PROD"] = m["SPIRA_HOME"]
text = open(sys.argv[1]).read()
if not m["DOLT"] and "@DOLT@" in text:
    sys.stderr.write("unit-ensure: %s: dolt is not on PATH\n" % os.path.basename(sys.argv[1]))
    raise SystemExit(1)
out = re.sub(r"@([A-Z_]+)@", lambda x: m.get(x.group(1), x.group(0)), text)
if watcher_name:
    out = out.replace("%i", watcher_name)
_tname = os.path.basename(sys.argv[1])
if _tname.startswith("spira-") and _tname.endswith(".timer"):
    out = re.sub(
        r"^(Unit=spira-[A-Za-z0-9_-]+)\.service$",
        r"\g<1>-" + m["SPIRA_INSTANCE"] + ".service",
        out, flags=re.MULTILINE,
    )
left = sorted(set(re.findall(r"@([A-Z_]+)@", out)))
if left:
    sys.stderr.write("unit-ensure: %s has unresolved placeholders: %s\n"
                     % (os.path.basename(sys.argv[1]), ", ".join(left)))
    raise SystemExit(1)
sys.stdout.write(out)
PY
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

if [ "$n_changed" -eq 0 ]; then
    printf 'unit-ensure: no changes — %d unit(s) current\n' "$n_unchanged"
    exit 0
fi

"$SC" --user daemon-reload
printf 'unit-ensure: daemon-reload after %d change(s), %d unchanged\n' "$n_changed" "$n_unchanged"

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
