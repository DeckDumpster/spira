#!/usr/bin/env bash
# czar.sh — czar fast pass: deterministic queue-stall detection on a 30s timer.
#
# czar.sh --pass    seven detectors, deterministic remedies or immediate inference fallback
#
# Reads only cheap sources: the landing.log tail since the last pass, the open batch
# record, landstate, CHECK7's last reason from sentinel.log, and at most 2 gh API
# calls (forge.sh batch-ci-status) for the open batch's run.
#
# STAGE AWARENESS. Each class has SPIRA_CZAR_STAGE_<CLASS> (shadow|act, default shadow).
# In shadow the pass writes CZAR-WOULD: lines to czar.log and changes nothing.
# In act the deterministic remedy executes directly; inference fires the czar immediately.
#
# INFERENCE FALLBACK. When no deterministic remedy matches, the pass files a czar-trigger
# bead (via incident.sh, deduped by SPIRA_INCIDENT_REF) and calls summon_fayth czar —
# the czar aeon starts without waiting for the next sentinel pass.
#
# TELEMETRY. Every pass writes one CLASS= line per detector class to czar.log. The
# cockpit reads these lines.
#
# covers: spira/czar.sh spira/sentinel.sh spira/watchtower.sh
#         spira/systemd/spira-czar-pass.service spira/systemd/spira-czar-pass.timer
set -uo pipefail
. "$(dirname "$0")/lib.sh"

[ "${1:-}" = "--pass" ] || { printf 'usage: czar.sh --pass\n' >&2; exit 2; }

[ -f "${SPIRA_RUN:-}/world.halted" ] && {
    log "czar pass: skipped — world is halted"
    exit 0
}

_lock="${SPIRA_RUN:-/tmp}/czar-pass.lock"
exec 9>"$_lock"
flock -n 9 || { log "czar pass: already running — skip"; exit 0; }

_czar_log="${SPIRA_CZAR_LOG:-$SPIRA_RUN/czar.log}"
_qc_log="${SPIRA_QUEUE_LOG:-$SPIRA_RUN/landing.log}"
_marker="${SPIRA_CZAR_PASS_MARKER:-$SPIRA_RUN/czar-pass.swept}"
_inc="${SPIRA_INCIDENT_SH:-$SPIRA_HOME/incident.sh}"
_forge="${SPIRA_FORGE:-$SPIRA_HOME/forge.sh}"
_queue_dir="${SPIRA_QUEUE_DIR:-$SPIRA_RUN/queue}"
_stall_secs="${SPIRA_LOOP_STALL_SECS:-3000}"
_ci_queued_max="${SPIRA_CI_QUEUED_MAX_SECS:-600}"
_starved_max_s=$(( ${SPIRA_STARVED_MAX_MINS:-20} * 60 ))
_strands_state="${SPIRA_STRANDS_STATE:-$SPIRA_RUN/strands.json}"
_ci_red_max="${SPIRA_CI_RED_MAX_SECS:-600}"

_now="$(date +%s)"
_now_iso="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

_prev="$(cat "$_marker" 2>/dev/null || true)"
_new=""
if [ -r "$_qc_log" ]; then
    if [ -n "$_prev" ]; then
        _new="$(awk -v s="$_prev" '$1 > s' "$_qc_log" 2>/dev/null || true)"
    else
        _new="$(tail -n 200 "$_qc_log" 2>/dev/null || true)"
    fi
fi

_check7_reason=""
[ -r "$SPIRA_RUN/sentinel.log" ] && \
    _check7_reason="$(grep 'CHECK7' "$SPIRA_RUN/sentinel.log" 2>/dev/null | tail -20 || true)"

# Stage for a class: shadow (default) or act
_stage() {
    local var="SPIRA_CZAR_STAGE_$(printf '%s' "$1" | tr '[:lower:]-' '[:upper:]_')"
    printf '%s' "${!var:-shadow}"
}

# First-seen: record when a fault first appeared in this pass history.
_fs_get()    { cat "$SPIRA_RUN/czar-pass-first.$(printf '%s' "$1" | tr '/,' '-')" 2>/dev/null || true; }
_fs_record() { local f="$SPIRA_RUN/czar-pass-first.$(printf '%s' "$1" | tr '/,' '-')"; [ -f "$f" ] || printf '%s\n' "$_now" > "$f" 2>/dev/null || true; }
_fs_clear()  { rm -f "$SPIRA_RUN/czar-pass-first.$(printf '%s' "$1" | tr '/,' '-')" 2>/dev/null || true; }

# Telemetry: one CLASS= line per detector class per pass.
_telem() {
    local class="$1" detected="$2" remedy="$3" tier="$4"
    local first latency=0
    first="$(_fs_get "$class")"
    case "${first:-}" in ''|*[!0-9]*) :;; *) latency=$(( _now - first ));; esac
    printf '%s CLASS=%s DETECTED=%s REMEDY=%s TIER=%s LATENCY=%ss\n' \
        "$_now_iso" "$class" "$detected" "$remedy" "$tier" "$latency" \
        >> "$_czar_log" 2>/dev/null || true
}

# File a czar-trigger bead and summon the czar immediately (inference path).
# In shadow: write CZAR-WOULD to czar.log; file nothing; summon nothing.
_infer() {
    local class="$1" ref="$2" subj="$3" body="$4"
    local stage; stage="$(_stage "$class")"
    case "$stage" in
        shadow)
            printf '%s CZAR-WOULD: %s inference — %s\n' "$_now_iso" "$class" "$subj" \
                >> "$_czar_log" 2>/dev/null || true
            ;;
        act)
            printf '%s\n' "$body" | \
            SPIRA_DB="$SPIRA_DB" \
            SPIRA_INCIDENT_LABELS="${SPIRA_SCOPE_LABEL:+$SPIRA_SCOPE_LABEL,}${SPIRA_CZAR_LABEL}" \
            SPIRA_INCIDENT_TYPE=task \
            SPIRA_INCIDENT_PRIORITY=1 \
            SPIRA_INCIDENT_ACTOR=czar-pass \
            SPIRA_SIN_EXEMPT=1 \
            SPIRA_INCIDENT_REPO=spira \
            SPIRA_INCIDENT_REF="incident:queue-${ref}" \
            SPIRA_INCIDENT_CAUSE="$class" \
            bash "$_inc" file "$subj" - >/dev/null 2>/dev/null || true
            log "czar pass: ${class} → inference (filed, summoning czar)"
            summon_fayth czar 2>/dev/null || true
            ;;
    esac
}

# Execute a deterministic remedy (act mode) or write CZAR-WOULD (shadow mode).
# $3 is an eval'd shell expression; keep it simple and never interpolate user data.
_det() {
    local class="$1" desc="$2" action="$3"
    local stage; stage="$(_stage "$class")"
    case "$stage" in
        shadow)
            printf '%s CZAR-WOULD: %s det — %s\n' "$_now_iso" "$class" "$desc" \
                >> "$_czar_log" 2>/dev/null || true
            ;;
        act)
            log "czar pass: ${class} → det: $desc"
            eval "$action" 2>/dev/null || true
            ;;
    esac
}

# ======================================================================================
# DETECTOR: deadlock — verdict left the batch open after a red with no attributable suite.
# Remedy: one full workflow rerun; inference if the fault persists past the first attempt.
# ======================================================================================
if printf '%s' "$_new" | grep -q 'no suites identified; leaving batch open'; then
    _fs_record deadlock
    _dl_ctx="$(printf '%s' "$_new" | grep 'no suites identified; leaving batch open' | tail -3)"
    _dl_body="$(printf 'verdict left the batch PR open after a red with no attributable suite.\nThe queue cannot advance until the batch is closed or requeued by hand.\n\nRecent log lines:\n%s\n' "$_dl_ctx")"
    _dl_first="$(_fs_get deadlock)"
    _dl_age=$(( _now - ${_dl_first:-$_now} ))
    _dl_run_id="" _dl_repo_path=""
    if [ "$_dl_age" -lt 90 ] && [ -d "$_queue_dir" ] && [ -r "$_forge" ]; then
        while IFS= read -r _dl_open; do
            [ -r "$_dl_open" ] || continue
            _dl_branch=""
            while IFS='=' read -r k v; do [ "$k" = branch ] && { _dl_branch="$v"; break; }; done < "$_dl_open"
            _dl_repo="$(basename "$(dirname "$_dl_open")")"
            [ -n "$_dl_branch" ] || continue
            _dl_repo_path="$(repo_root "$_dl_repo" 2>/dev/null)" || true
            [ -n "$_dl_repo_path" ] || continue
            _dl_run_id="$(bash "$_forge" run-id "$_dl_repo_path" "$_dl_branch" 2>/dev/null)" || true
            [ -n "$_dl_run_id" ] && break
        done < <(find "$_queue_dir" -maxdepth 2 -name "open" -type f 2>/dev/null)
    fi
    if [ -n "$_dl_run_id" ] && [ "$_dl_age" -lt 90 ]; then
        _det deadlock "workflow-rerun $_dl_run_id" \
            "bash \"$_forge\" workflow-rerun \"$_dl_repo_path\" \"$_dl_run_id\""
        _telem deadlock yes det-rerun det
    else
        _infer deadlock "deadlock-batch-open" \
            "QUEUE: batch open — no suites identified (DEADLOCK)" "$_dl_body"
        _telem deadlock yes inference inf
    fi
else
    _fs_clear deadlock
    _telem deadlock no none det
fi

# ======================================================================================
# DETECTOR: attribution-failed — attribution ejected 0 survivors; whole batch requeued.
# Remedy: inference (the czar investigates and isolates the offender).
# ======================================================================================
if printf '%s' "$_new" | grep -qE 'ejected 0, requeued [1-9]'; then
    _fs_record attribution-failed
    _af_ctx="$(printf '%s' "$_new" | grep -E 'ejected 0, requeued [1-9]' | tail -3)"
    _infer attribution-failed "attribution-failed-requeue" \
        "QUEUE: attribution ejected 0, requeued whole batch" \
        "$(printf 'Attribution found no branch to isolate the offender.\nThe entire batch was requeued with the failing member still in it; this loops.\n\nRecent log lines:\n%s\n' "$_af_ctx")"
    _telem attribution-failed yes inference inf
else
    _fs_clear attribution-failed
    _telem attribution-failed no none det
fi

# ======================================================================================
# DETECTOR: sort-failed — queue_sort_rows ranking failed; priority ordering suspended.
# Remedy: inference (the czar diagnoses and restores the sorter).
# ======================================================================================
if printf '%s' "$_new" | grep -q 'queue_sort_rows: ranking failed'; then
    _fs_record sort-failed
    _sf_ctx="$(printf '%s' "$_new" | grep 'queue_sort_rows: ranking failed' | tail -3)"
    _infer sort-failed "sort-failed-ranking" \
        "QUEUE: queue_sort_rows ranking failed" \
        "$(printf 'The queue sorter failed and fell back to unranked order.\nPriority ordering is suspended until the sorter recovers.\n\nRecent log lines:\n%s\n' "$_sf_ctx")"
    _telem sort-failed yes inference inf
else
    _fs_clear sort-failed
    _telem sort-failed no none det
fi

# ======================================================================================
# DETECTOR: loop-stalled — no landing pass completed within SPIRA_LOOP_STALL_SECS.
# Remedy: reset-failed + start if spira-landing.service is failed; inference if running.
# ======================================================================================
_ls_last_ts=""
[ -r "$_qc_log" ] && \
    _ls_last_ts="$(grep 'landing: pass complete' "$_qc_log" 2>/dev/null | tail -1 | cut -c1-20)"
if [ -n "$_ls_last_ts" ]; then
    _ls_age=$(( _now - $(date -u -d "$_ls_last_ts" +%s 2>/dev/null || echo "$_now") ))
    if [ "$_ls_age" -gt "$_stall_secs" ] 2>/dev/null; then
        _fs_record loop-stalled
        _ls_unit="${SPIRA_LAND_UNIT:-spira-landing}"
        _sc="${SPIRA_SYSTEMCTL:-systemctl}"
        if [ "$("$_sc" --user is-failed "${_ls_unit}.service" 2>/dev/null || true)" = failed ]; then
            _det loop-stalled "reset-failed + start ${_ls_unit}" \
                "\"$_sc\" --user reset-failed \"${_ls_unit}.service\" && \"$_sc\" --user start \"${_ls_unit}.service\""
            _telem loop-stalled yes det-restart det
        else
            _infer loop-stalled "loop-stalled" \
                "QUEUE: landing loop stalled — no pass for ${_ls_age}s" \
                "$(printf 'No landing: pass complete in the last %ds (threshold: %ds).\nLast pass: %s\n\nCheck: %s --user status %s\nSee: %s\n' \
                    "$_ls_age" "$_stall_secs" "$_ls_last_ts" "$_sc" "$_ls_unit" "$_qc_log")"
            _telem loop-stalled yes inference inf
        fi
    else
        _fs_clear loop-stalled
        _telem loop-stalled no none det
    fi
else
    _telem loop-stalled no none det
fi

# ======================================================================================
# DETECTORS: ci-stalled + ci-red (share 2 gh API calls via batch-ci-status)
#
# ci-stalled: a CI job has been queued with no runner > SPIRA_CI_QUEUED_MAX_SECS.
#   Remedy: cancel the stuck run and re-dispatch the whole workflow (never rerun --failed).
# ci-red: CI run completed failure and red for > SPIRA_CI_RED_MAX_SECS (verdict not acting).
#   Measures from run-completed-at (CI completion), not from batch open.
#   Remedy: inference.
# ======================================================================================
_cis_detected=no _cis_remedy=none _cis_tier=det
_cir_detected=no _cir_remedy=none _cir_tier=det

if [ -d "$_queue_dir" ] && [ -r "$_forge" ]; then
    while IFS= read -r _ci_open; do
        [ -r "$_ci_open" ] || continue
        _ci_branch=""
        while IFS='=' read -r _k _v; do [ "$_k" = branch ] && { _ci_branch="$_v"; break; }; done < "$_ci_open"
        [ -n "$_ci_branch" ] || continue
        _ci_repo="$(basename "$(dirname "$_ci_open")")"
        _ci_repo_path="$(repo_root "$_ci_repo" 2>/dev/null)" || true
        [ -n "$_ci_repo_path" ] || continue

        _ci_out="$(bash "$_forge" batch-ci-status "$_ci_repo_path" "$_ci_branch" 2>/dev/null)" || _ci_out=""
        _ci_run_id="$(printf '%s' "$_ci_out" | sed -n 's/^run-id: //p')"
        _ci_qs="$(printf '%s' "$_ci_out" | sed -n 's/^queued-since: //p')"

        if [ -n "$_ci_qs" ]; then
            case "$_ci_qs" in
                ''|*[!0-9]*) ;;
                *)
                    _ci_stalled_s=$(( _now - _ci_qs ))
                    if [ "$_ci_stalled_s" -gt "$_ci_queued_max" ] 2>/dev/null; then
                        _cis_detected=yes
                        _fs_record ci-stalled
                        _ci_s_safe="${_ci_repo//,/-}"
                        if [ -n "$_ci_run_id" ]; then
                            _det ci-stalled \
                                "workflow-rerun $_ci_run_id (${_ci_repo}, queued ${_ci_stalled_s}s)" \
                                "bash \"$_forge\" workflow-rerun \"$_ci_repo_path\" \"$_ci_run_id\""
                            _cis_remedy=det-rerun
                        else
                            _infer ci-stalled "ci-stalled-${_ci_s_safe}" \
                                "QUEUE: CI job queued with no runner for ${_ci_stalled_s}s (${_ci_repo})" \
                                "$(printf 'A CI job for the open batch in %s has been queued for %ds (threshold: %ds).\nBranch: %s\n\nCancel the stuck run and re-dispatch the whole workflow.\nNever rerun --failed: that strands the run on the torn-down VM label.\n' \
                                    "$_ci_repo" "$_ci_stalled_s" "$_ci_queued_max" "$_ci_branch")"
                            _cis_remedy=inference
                            _cis_tier=inf
                        fi
                        continue
                    fi
                    ;;
            esac
        fi

        _ci_conclusion="$(printf '%s' "$_ci_out" | sed -n 's/^run-conclusion: //p')"
        _ci_completed_at="$(printf '%s' "$_ci_out" | sed -n 's/^run-completed-at: //p')"
        if [ "$_ci_conclusion" = "failure" ]; then
            case "${_ci_completed_at:-}" in
                ''|*[!0-9]*) : ;;
                *)
                    _ci_red_age=$(( _now - _ci_completed_at ))
                    if [ "$_ci_red_age" -gt "$_ci_red_max" ] 2>/dev/null; then
                        _cir_detected=yes
                        _fs_record ci-red
                        _ci_r_safe="${_ci_repo//,/-}"
                        _infer ci-red "ci-red-${_ci_r_safe}" \
                            "QUEUE: CI run completed red, verdict not acting for ${_ci_red_age}s (${_ci_repo})" \
                            "$(printf 'CI run completed red for the open batch in %s.\nRed for %ds (threshold %ds); verdict has not acted.\nBranch: %s\nInvestigate the red and act.\n' \
                                "$_ci_repo" "$_ci_red_age" "$_ci_red_max" "$_ci_branch")"
                        _cir_remedy=inference
                        _cir_tier=inf
                        continue
                    fi
                    ;;
            esac
        fi
    done < <(find "$_queue_dir" -maxdepth 2 -name "open" -type f 2>/dev/null)
fi

[ "$_cis_detected" = no ] && _fs_clear ci-stalled
[ "$_cir_detected" = no ] && _fs_clear ci-red
_telem ci-stalled "$_cis_detected" "$_cis_remedy" "$_cis_tier"
_telem ci-red "$_cir_detected" "$_cir_remedy" "$_cir_tier"

# ======================================================================================
# DETECTOR: starved — a partition has ready work with no serving aeons for too long.
# Remedy:
#   throttle-stamp reason  → recompute depth via watchtower --throttle-check
#   suspended/held reason  → deliberate state (law-a-deliberate-state-is-not-a-fault)
#   other                  → inference
# ======================================================================================
_sv_detected=no _sv_remedy=none _sv_tier=det

if [ -r "$_strands_state" ]; then
    while IFS=$'\t' read -r _sv_part _sv_first; do
        [ -n "$_sv_part" ] || continue
        case "${_sv_first:-}" in ''|*[!0-9]*) continue ;; esac
        _sv_age=$(( _now - _sv_first ))
        if [ "$_sv_age" -gt "$_starved_max_s" ] 2>/dev/null; then
            _sv_detected=yes
            _sv_safe="${_sv_part//,/-}"; _sv_safe="${_sv_safe// /-}"
            _fs_record "starved"
            _sv_stage="$(_stage starved)"
            case "$_sv_stage" in
                shadow)
                    printf '%s CZAR-WOULD: starved inference — partition [%s] starved %dm\n' \
                        "$_now_iso" "$_sv_part" "$(( _sv_age / 60 ))" \
                        >> "$_czar_log" 2>/dev/null || true
                    _sv_remedy=shadow-inference
                    _sv_tier=inf
                    ;;
                act)
                    _tc_stamp="${SPIRA_THROTTLE_STAMP:-$SPIRA_RUN/queue-throttled}"
                    if [ -f "$_tc_stamp" ] && printf '%s' "$_check7_reason" | grep -q 'throttle active'; then
                        bash "$SPIRA_HOME/watchtower.sh" --throttle-check 2>/dev/null || true
                        log "czar pass: starved → recomputed throttle depth (partition: $_sv_part)"
                        _sv_remedy=det-throttle-recompute
                    elif printf '%s' "$_check7_reason" | grep -qE 'SPIRA_MAX_AEONS=0|world\.halted|suspended'; then
                        log "czar pass: starved → deliberate state (partition: $_sv_part)"
                        _sv_remedy=deliberate-state
                    else
                        _infer starved "starved-${_sv_safe}" \
                            "QUEUE: partition [${_sv_part}] starved for $(( _sv_age / 60 ))m" \
                            "$(printf 'Ready work in partition [%s] has had no serving aeons for %dm (threshold: %dm).\n\nCHECK7 reason (sentinel.log):\n%s\n' \
                                "$_sv_part" "$(( _sv_age / 60 ))" "$(( _starved_max_s / 60 ))" \
                                "$(printf '%s' "$_check7_reason" | tail -5)")"
                        _sv_remedy=inference
                        _sv_tier=inf
                    fi
                    ;;
            esac
        fi
    done < <(python3 -c "
import json, sys
try:
    with open(sys.argv[1]) as fh: st = json.load(fh)
    for key, val in st.items():
        parts = key.split(':', 2)
        if len(parts) == 3 and parts[1] == 'starved':
            first = val.get('first')
            if first:
                print(parts[0] + '\t' + str(int(first)))
except Exception: pass
" "$_strands_state" 2>/dev/null)
fi

[ "$_sv_detected" = no ] && _fs_clear starved
_telem starved "$_sv_detected" "$_sv_remedy" "$_sv_tier"

printf '%s\n' "$_now_iso" > "$_marker" 2>/dev/null || true
log "czar pass: complete ($(( $(date +%s) - _now ))s)"
