#!/usr/bin/env bash
#
# collect.sh — tiered cockpit collector supervisor
#
#   collect.sh [loop]   run the supervisor (requires supervision; checks INVOCATION_ID)
#   collect.sh merge    merge cockpit.d/*.env into cockpit.env once (no supervision check)
#
# WHAT THIS DOES
# --------------
# Replaces the single pass in cockpit.sh loop with a probe registry. Each probe runs in
# the background on its own schedule and writes an atomic fragment under
# $SPIRA_RUN/cockpit.d/<probe>.env. The supervisor merges all fragments into
# $SPIRA_RUN/cockpit.env on every 5s tick so existing readers that expect that file see
# fresher values without waiting on the slowest probe.
#
# THREE TIERS, CHOSEN BY WHAT A PROBE TOUCHES (not by how fast it happened to be):
#   fast   (5s)    filesystem only — PIDs, lease files, timer states, gate-run/
#   medium (60s)   one bounded database query or log-file scan per call
#   slow   (600s)  graph walks, git fetches, anything unbounded
#
# FRAGMENT LIFECYCLE:
#   never   — probe has not yet had a successful run; fragment has _PROBE_STATUS=never
#             and NO value keys, so those keys are absent from cockpit.env and render
#             '?' through the shell defaults in health.sh
#   ok      — last run succeeded; fragment carries values and _PROBE_STATUS=ok
#   stale   — last run failed; fragment RETAINS old values with _PROBE_STATUS=stale,
#             so the pane still shows the last known values annotated with their age
#             rather than blanking them (blanking reads as zero on a pane that lays
#             rows by position — the defect sp-xrkuu fixed)
#
# SKIP, NOT QUEUE. A probe still running when its next slot arrives is skipped. The skip
# is visible as growing age on that probe's values rather than as a silent backlog.
#
# CPU FENCE (law-fence-loops-on-shared-hardware). The service's CPUQuota covers the whole
# supervisor tree. The slow tier is further bounded to SLOW_CONCURRENT concurrent jobs
# so one expensive graph walk cannot crowd out the fast tier's 5s repaint.
#
# MIGRATION: keeps writing the merged cockpit.env so existing readers and test suites
# that source that file do not break when the supervisor replaces cockpit.sh loop.
set -uo pipefail
. "$(dirname "$0")/lib.sh"

HERE="$(cd "$(dirname "$0")" && pwd)"
# COCK may be overridden from the environment for testing; normal operation uses cockpit.sh.
: "${COCK:=$HERE/cockpit.sh}"

SNAP="$SPIRA_RUN/cockpit.env"
# FRAG_DIR may be overridden from the environment for testing.
: "${FRAG_DIR:=$SPIRA_RUN/cockpit.d}"
TICK="${SPIRA_COCKPIT_TICK:-5}"

WINDOW_HOURS="${SPIRA_COCKPIT_WINDOW_HOURS:-24}"

_MERGE_FAIL_MAX="${SPIRA_COCKPIT_MERGE_FAIL_MAX:-3}"

# PROBE REGISTRY: "name:interval_s:timeout_s:subcommand"
# Interval classifies tier (fast=5, medium=60, slow=600).
PROBES=(
    "now:5:30:now"
    "sphere:60:90:sphere"
    "repo_labels:60:90:repo_labels"
    "strands:60:90:strands"
    "ratelim:60:90:ratelim"
    "core:60:150:core"
    "core_detail:600:900:core_detail"
    "sops:600:300:sops"
    "livelock:600:300:livelock"
    "dup_refs:600:300:dup_refs"
    "unsent:600:300:unsent"
)

# Maximum concurrent slow-tier (interval>=600) probes. The service CPUQuota caps the
# whole tree; this caps the slow-tier share within that budget.
SLOW_CONCURRENT="${SPIRA_COCKPIT_SLOW_CONCURRENT:-1}"

# Same supervision check as cockpit.sh: only the process systemd started for this
# service may write to the snapshot. SPIRA_COCKPIT_FORCE=1 names the override.
collect_may_write() {
    [ "${SPIRA_COCKPIT_FORCE:-0}" = 1 ] && return 0
    [ -n "${INVOCATION_ID:-}" ] || return 1
    local svc_id u
    for u in "spira-cockpit${SPIRA_INSTANCE:+-$SPIRA_INSTANCE}.service" spira-cockpit.service; do
        svc_id="$(systemctl --user show "$u" -p InvocationID --value 2>/dev/null)" || continue
        [ -n "$svc_id" ] && [ "$INVOCATION_ID" = "$svc_id" ] && return 0
    done
    return 1
}

# Write an initial "never" fragment for a probe so the merge produces '?' for its keys
# from the very first tick, before any successful run. Leaves an existing fragment alone
# — a supervisor restart must not overwrite a previous session's last-known-good values.
_write_never_frag() {
    local name="$1" frag="$FRAG_DIR/${name}.env"
    [ -f "$frag" ] && return 0
    printf '_PROBE_AT=0\n_PROBE_STATUS=never\n' > "${frag}.tmp.$$" \
        && mv "${frag}.tmp.$$" "$frag" || rm -f "${frag}.tmp.$$"
}

# Run one probe, write its fragment. Called in a subshell (background); exit status
# is the probe's own exit status.
#
# On success: write a fresh fragment with _PROBE_STATUS=ok and all output keys.
# On failure: keep the previous fragment's values but set _PROBE_STATUS=stale
#   so the pane still renders last-known-good values with their age annotated.
#   A probe whose previous status was 'never' stays 'never' — no good values to keep.
_run_probe_body() {
    local name="$1" timeout_s="$2" cmd="$3"
    local frag="$FRAG_DIR/${name}.env"
    local now; now=$(date +%s)
    local out_tmp; out_tmp="$(mktemp "$FRAG_DIR/.${name}.XXXXXX")" || return 1

    if timeout "$timeout_s" bash "$COCK" "$cmd" > "$out_tmp" 2>/dev/null; then
        local hdr_tmp; hdr_tmp="$(mktemp "$FRAG_DIR/.${name}.XXXXXX")" || { rm -f "$out_tmp"; return 1; }
        { printf '_PROBE_AT=%s\n_PROBE_STATUS=ok\n' "$now"; cat "$out_tmp"; } > "$hdr_tmp" \
            && mv "$hdr_tmp" "$frag"
        rm -f "$out_tmp"
    else
        local rc=$?
        rm -f "$out_tmp"
        local prev_at="0" prev_status="never"
        if [ -f "$frag" ]; then
            prev_at="$(awk -F= '/^_PROBE_AT=/{print $2; exit}' "$frag" 2>/dev/null)" || prev_at="0"
            prev_status="$(awk -F= '/^_PROBE_STATUS=/{print $2; exit}' "$frag" 2>/dev/null)" || prev_status="never"
        fi
        if [ "${prev_status:-never}" = "never" ]; then
            # First-run failure: write a fault fragment so the pane can distinguish
            # "has not run yet" (never) from "was killed before producing output"
            # (timeout or error). _PROBE_AT stays 0 — no successful run has set it.
            local fault_status="error"
            [ "$rc" -eq 124 ] && fault_status="timeout"
            printf 'collect.sh: probe %s %s after %ss\n' "$name" "$fault_status" "$timeout_s" >&2
            local fault_tmp; fault_tmp="$(mktemp "$FRAG_DIR/.${name}.XXXXXX")" || return "$rc"
            printf '_PROBE_AT=%s\n_PROBE_STATUS=%s\n' "${prev_at:-0}" "$fault_status" \
                > "$fault_tmp" && mv "$fault_tmp" "$frag" || rm -f "$fault_tmp"
            return "$rc"
        fi
        local stale_tmp; stale_tmp="$(mktemp "$FRAG_DIR/.${name}.XXXXXX")" || return "$rc"
        {
            printf '_PROBE_AT=%s\n_PROBE_STATUS=stale\n' "${prev_at:-0}"
            grep -v '^_PROBE_AT=\|^_PROBE_STATUS=' "$frag" 2>/dev/null
        } > "$stale_tmp" && mv "$stale_tmp" "$frag" || rm -f "$stale_tmp"
        return "$rc"
    fi
}

# Merge all cockpit.d/*.env fragments into cockpit.env.
#
# PER-PROBE METADATA is emitted first as _PROBE_AT_<name> and _PROBE_STATUS_<name>
# so health.sh can annotate individual values without a global STALE banner.
#
# VALUE KEYS use first-wins across fragments sorted alphabetically by probe name.
# The fast probe 'now' (n < s for sops etc.) delivers SP_AT and the aeon picture
# on every 5s tick; slow probe values sit untouched until those probes refresh.
#
# 'never' probes contribute no value keys — their keys are absent from cockpit.env
# and the renderer sees the shell default ('?') for each.
_merge_fragments() {
    mkdir -p "$FRAG_DIR"
    local snap_tmp; snap_tmp="$(mktemp "$SPIRA_RUN/.cockpit.XXXXXX")" || return 1

    python3 - "$FRAG_DIR" "$snap_tmp" <<'PY' 2>/dev/null || { rm -f "$snap_tmp"; return 1; }
import sys, os, glob

frag_dir, snap_tmp = sys.argv[1], sys.argv[2]

meta_lines  = []   # _PROBE_AT_name=, _PROBE_STATUS_name= — emitted first
value_seen  = set()
value_lines = []   # SP_* and other value keys — first-wins

for frag_path in sorted(glob.glob(os.path.join(frag_dir, "*.env"))):
    name = os.path.basename(frag_path)[:-4]
    probe_at     = "0"
    probe_status = "never"
    val_pairs = []
    try:
        for line in open(frag_path, errors="replace"):
            line = line.rstrip("\n")
            if "=" not in line:
                continue
            k, _, v = line.partition("=")
            k = k.strip()
            if k == "_PROBE_AT":
                probe_at = v
            elif k == "_PROBE_STATUS":
                probe_status = v
            elif k and (k[0].isalpha() or k[0] == "_"):
                val_pairs.append((k, v))
    except OSError:
        pass
    meta_lines.append("_PROBE_AT_%s=%s"     % (name, probe_at))
    meta_lines.append("_PROBE_STATUS_%s=%s" % (name, probe_status))
    if probe_status not in ("never", "timeout", "error"):
        for k, v in val_pairs:
            if k not in value_seen:
                value_seen.add(k)
                value_lines.append((k, v))

def shq(v):
    return "'" + v.replace("'", "'\\''") + "'"

with open(snap_tmp, "w") as f:
    for line in meta_lines:
        k, _, v = line.partition("=")
        f.write("%s=%s\n" % (k, shq(v)))
    for k, v in value_lines:
        f.write("%s=%s\n" % (k, shq(v)))
PY
    mv "$snap_tmp" "$SNAP" 2>/dev/null
}

# The supervisor loop: tick every TICK seconds, start due probes, merge fragments.
_supervisor_loop() {
    mkdir -p "$FRAG_DIR"
    local entry name
    for entry in "${PROBES[@]}"; do
        name="${entry%%:*}"
        _write_never_frag "$name"
    done

    # Record config file mtime at startup; exit cleanly when it changes so the
    # restart re-reads the updated config (law-long-lived-processes-pin-their-config).
    local _conf_file="${SPIRA_CONF_FILE:-}"
    local _conf_mtime_0
    _conf_mtime_0="$(stat --format='%Y' "$_conf_file" 2>/dev/null || echo 0)"

    declare -A PROBE_PID=()
    declare -A PROBE_LAST=()
    local _merge_fail=0

    while :; do
        # Watchdog heartbeat: keeps systemd from killing a live supervisor between ticks.
        # Requires WatchdogSec= and NotifyAccess=main in the unit.
        [ -n "${NOTIFY_SOCKET:-}" ] && systemd-notify --watchdog 2>/dev/null || true

        # Config-change check: exit cleanly so the restart picks up the new config.
        if [ -n "$_conf_file" ]; then
            local _conf_mtime_now
            _conf_mtime_now="$(stat --format='%Y' "$_conf_file" 2>/dev/null || echo 0)"
            if [ "$_conf_mtime_now" != "$_conf_mtime_0" ]; then
                printf 'collect.sh: config changed — exiting for restart\n' >&2
                exit 0
            fi
        fi

        local _now; _now=$(date +%s)
        local slow_running=0

        # Count currently-running slow probes (interval >= 600).
        for entry in "${PROBES[@]}"; do
            name="${entry%%:*}"; local rest="${entry#*:}"
            local interval="${rest%%:*}"
            local pid="${PROBE_PID[$name]:-}"
            if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then
                [ "$interval" -ge 600 ] && slow_running=$((slow_running+1))
            else
                unset "PROBE_PID[$name]" 2>/dev/null || true
            fi
        done

        # Start probes whose interval has elapsed and that are not already running.
        for entry in "${PROBES[@]}"; do
            name="${entry%%:*}"; rest="${entry#*:}"
            interval="${rest%%:*}"; rest="${rest#*:}"
            local timeout_s="${rest%%:*}"
            local cmd="${rest#*:}"
            local last="${PROBE_LAST[$name]:-0}"
            local pid="${PROBE_PID[$name]:-}"

            # Skip if still running — a slow probe holds its slot; skip is visible as
            # growing age rather than a silent backlog (see design).
            [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null && continue

            # Skip if not yet due.
            [ $((_now - last)) -lt "$interval" ] && continue

            # Limit concurrent slow probes.
            if [ "$interval" -ge 600 ] && [ "$slow_running" -ge "$SLOW_CONCURRENT" ]; then
                continue
            fi

            ( _run_probe_body "$name" "$timeout_s" "$cmd" ) &
            PROBE_PID[$name]=$!
            PROBE_LAST[$name]=$_now
            [ "$interval" -ge 600 ] && slow_running=$((slow_running+1))
        done

        if _merge_fragments; then
            _merge_fail=0
        else
            _merge_fail=$((_merge_fail + 1))
            if [ "$_merge_fail" -ge "$_MERGE_FAIL_MAX" ]; then
                printf 'collect.sh: %d consecutive merge failures — exiting for restart\n' \
                    "$_MERGE_FAIL_MAX" >&2
                exit 1
            fi
        fi

        sleep "$TICK"
    done
}

# Remove stale .cockpit.* temps left by a previous unclean exit (same as cockpit.sh).
_sweep_tmps() {
    local _f _n=0
    for _f in "$SPIRA_RUN"/.cockpit.*; do
        [ -e "$_f" ] || continue
        rm -f -- "$_f" && _n=$((_n+1))
    done
    [ "$_n" -gt 0 ] && printf 'collect.sh: swept %d stale temp(s)\n' "$_n" >&2
}

case "${1:-loop}" in
loop)
    collect_may_write || {
        printf 'collect.sh loop: write refused — not the supervised process. Set SPIRA_COCKPIT_FORCE=1 to override.\n' >&2
        exit 1
    }
    _sweep_tmps
    _supervisor_loop
    ;;
merge)
    _merge_fragments
    printf 'collect.sh: merged %d fragments into %s\n' \
        "$(ls "$FRAG_DIR"/*.env 2>/dev/null | wc -l)" "$SNAP"
    ;;
_probe_body_test)
    # Test-only: run _run_probe_body with caller-supplied args and exit with its status.
    # FRAG_DIR and COCK must be set in the environment. Intended only for test suites;
    # the subcommand name's leading _ keeps it out of the usage line.
    shift
    _run_probe_body "$@"
    ;;
*)
    printf 'usage: collect.sh [loop|merge]\n' >&2; exit 1 ;;
esac
