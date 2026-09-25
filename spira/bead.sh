#!/usr/bin/env bash
#
# bead.sh — file a bead through the contract; never call bd create directly.
#
#   bead.sh file "<title>" --for <persona> --repo <name> [--priority N] [--body-file F]
#   bead.sh file "<title>" --kind <kind> [--repo <name>] [--priority N] [--body-file F]
#   bead.sh lint [--all|<id>...]     check that beads in the store satisfy the contract
#   bead.sh contract                 legal personas, repos and kinds, read from source
#
# WHY THIS EXISTS AND NOT bd create DIRECTLY
# ------------------------------------------
# The partition labels a bead carries come from the persona's own predicate; writing them by
# hand means they can disagree, and a bead with wrong labels is either invisible (the persona
# cannot find it) or misrouted (a different persona claims it). `--for <persona>` reads the
# label set from the fayth file, so the filing tool and the claim predicate cannot disagree.
#
# For non-work kinds (event, escalation, proposal, insight) no persona claims the bead, so
# --for is not valid. The label set carries the scope label and, if --repo is given, repo:<name>;
# no partition label is added, making the bead deliberately unclaimable rather than accidentally
# invisible. insight beads are created closed at P4 per the schema declaration.
#
# `bead.sh contract` asks the live source — fayths for personas, schema.sh for kinds, the
# repo-map for repos — so the set it prints is exactly the set it can accept.
set -uo pipefail
BEAD_HOME="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$BEAD_HOME/lib.sh"

_bead_file() {
    local title="${1:-}"; shift || true
    [ -n "$title" ] || { printf 'bead: title required\n' >&2; return 2; }
    local for_fayth="" repo="" priority="" body_file="" kind="" express=""
    while [ $# -gt 0 ]; do
        case "$1" in
            --for)          shift; for_fayth="${1:-}" ;;
            --repo)         shift; repo="${1:-}" ;;
            --priority|-p)  shift; priority="${1:-}" ;;
            --body-file)    shift; body_file="${1:-}" ;;
            --kind)         shift; kind="${1:-}" ;;
            --express)      express=1 ;;
            *) printf 'bead: unknown option: %s\n' "$1" >&2; return 2 ;;
        esac
        shift
    done

    # A repo: label absent from the map is refused here, before bd is ever called, so
    # no bead is filed for a repository the harness has no row for
    # (law-a-refusal-names-its-exit). Read through repo_names(), the same accessor
    # every other repo-scoped lookup in the harness uses.
    if [ -n "$repo" ] && ! repo_names 2>/dev/null | grep -qxF "$repo"; then
        local _valid; _valid="$(repo_names 2>/dev/null | sort | tr '\n' ' ' | sed 's/ $//')"
        printf 'bead: repo:%s is not in the repo map; valid keys: %s\n' \
            "$repo" "${_valid:-<map not found>}" >&2
        return 2
    fi

    # --kind and --for are mutually exclusive routing decisions
    if [ -n "$kind" ] && [ -n "$for_fayth" ]; then
        printf 'bead: --kind and --for are mutually exclusive\n' >&2; return 2
    fi

    # Default to work kind when --for is used (legacy path)
    [ -z "$kind" ] && kind="work"

    # gate is a bd-internal type; its fields (await_type, await_id, timeout) require bd gate
    [ "$kind" = "gate" ] && { printf 'bead: kind=gate is a bd-internal type; use bd gate directly\n' >&2; return 2; }

    local bd_type; bd_type="$("$BEAD_HOME/schema.sh" type-of "$kind" 2>/dev/null)" \
        || { printf 'bead: unknown kind: %s\n' "$kind" >&2; return 2; }

    if [ "$kind" = "work" ]; then
        [ -n "$for_fayth" ] || { printf 'bead: --for <persona> required\n' >&2; return 2; }
        [ -n "$repo" ]      || { printf 'bead: --repo <name> required\n' >&2; return 2; }
        local fpath="$SPIRA_HOME/chamber/$for_fayth.fayth"
        [ -f "$fpath" ] || { printf 'bead: no such persona: %s\n' "$for_fayth" >&2; return 2; }
        local labels; labels="$(fayth_get "$for_fayth" FAYTH_LABELS "")"
        [ -n "$labels" ] || { printf 'bead: persona %s has no partition labels\n' "$for_fayth" >&2; return 2; }
        # LANE CHECK. The repo must admit the persona's partition lane.
        # Override: SPIRA_BEAD_LANE_OVERRIDE=1
        if [ -z "${SPIRA_BEAD_LANE_OVERRIDE:-}" ]; then
            local _p="${SPIRA_PLAN_LABEL:-plan}"
            # literal-ok: bash fallbacks; conf.sh always sets SPIRA_MAECHEN_LABEL before this runs
            local _vocab="${_p} ${SPIRA_INCIDENT_LABEL:-incident} ${SPIRA_GROOMER_LABEL:-groom} ${SPIRA_MAECHEN_LABEL:-maechen-sweep} ${SPIRA_SPIKE_LABEL:-spike} ${SPIRA_CZAR_LABEL:-czar-trigger}"
            local _partition="" _lbl _ifs="$IFS"
            IFS=,
            for _lbl in $labels; do
                IFS="$_ifs"
                _lbl="${_lbl#"${_lbl%%[![:space:]]*}"}"; _lbl="${_lbl%"${_lbl##*[![:space:]]}"}"
                case " $_vocab " in *" $_lbl "*) _partition="$_lbl" ;; esac
                IFS=,
            done
            IFS="$_ifs"
            if [ -n "$_partition" ]; then
                local _repo_lanes
                _repo_lanes="$(spira_repo_lanes "$repo" 2>/dev/null)" || _repo_lanes="$_p"
                case " $_repo_lanes " in
                    *" $_partition "*) ;;
                    *)
                        local _raw; _raw="$(repo_field "$repo" lanes 2>/dev/null)"
                        local _refuser
                        if [ -n "$_raw" ]; then
                            _refuser="repo-map (lanes=${_raw})"
                        else
                            _refuser="repo-map (no lanes column — defaults to ${_p})"
                        fi
                        printf 'bead: repo:%s does not admit lane %s — refused by %s\n' \
                            "$repo" "$_partition" "$_refuser" >&2
                        printf 'bead: override: SPIRA_BEAD_LANE_OVERRIDE=1\n' >&2
                        return 2 ;;
                esac
            fi
        fi
        labels="$labels,repo:$repo"
        local _express_label="${SPIRA_EXPRESS_LABEL:-express}"
        [ -n "$express" ] && labels="$labels,$_express_label"
        set -- create "$title" -l "$labels"
        [ -n "$priority" ]  && set -- "$@" -p "$priority"
        [ -n "$body_file" ] && set -- "$@" --body-file "$body_file"
        bdq "$@"
    else
        # Non-work kind: no persona, no partition labels — deliberately unclaimable
        local scope_label; scope_label="$("$BEAD_HOME/schema.sh" name scope)"
        local labels="$scope_label"
        if [ "$kind" = "insight" ]; then
            local insight_label; insight_label="$("$BEAD_HOME/schema.sh" name insight)"
            labels="$labels,$insight_label"
        fi
        [ -n "$repo" ] && labels="$labels,repo:$repo"
        local _express_label="${SPIRA_EXPRESS_LABEL:-express}"
        [ -n "$express" ] && labels="$labels,$_express_label"
        set -- create "$title" -l "$labels" --type "$bd_type"
        [ "$kind" = "insight" ] && set -- "$@" --status closed
        [ "$kind" = "insight" ] && [ -z "$priority" ] && priority=4
        [ -n "$priority" ]  && set -- "$@" -p "$priority"
        [ -n "$body_file" ] && set -- "$@" --body-file "$body_file"
        bdq "$@"
    fi
}

_bead_contract() {
    printf 'PERSONAS\n'
    local f labels
    for f in $(fayth_names 2>/dev/null); do
        labels="$(fayth_get "$f" FAYTH_LABELS '' 2>/dev/null)"
        printf '  %-14s %s\n' "$f" "${labels:-(no labels)}"
    done
    printf '\nKINDS\n'
    "$BEAD_HOME/schema.sh" kinds
    printf '\nREPOS\n'
    if [ -f "${SPIRA_REPO_MAP:-}" ]; then
        grep -v '^[[:space:]]*#' "$SPIRA_REPO_MAP" | grep -v '^[[:space:]]*$' \
            | awk '{print "  " $1}'
    else
        printf '  (no repo-map)\n'
    fi
}

_bead_lint() {
    local rc=0 n=0 bad=0 id labels show_out show_rc show_parsed bead_status bead_type
    local ids=""
    if [ "${1:-}" = "--all" ] || [ $# -eq 0 ]; then
        ids="$(bdq list --all --limit 0 --json 2>/dev/null \
            | python3 -c '
import json, sys
data = json.load(sys.stdin)
for d in (data if isinstance(data, list) else [data]):
    if d.get("issue_type") != "event":
        print(d["id"])
' 2>/dev/null || true)"
    else
        ids="$*"
    fi

    # Partition labels from the chamber — open work beads must carry one (or no-loop).
    local _part="" _f _lbl _l _p _scope="${SPIRA_SCOPE_LABEL:-}" _no_loop="${SPIRA_NO_LOOP_LABEL:-}"
    local _ifs_save="$IFS"
    for _f in $(fayth_names 2>/dev/null); do
        _lbl="$(fayth_get "$_f" FAYTH_LABELS "" 2>/dev/null)"
        [ -n "$_lbl" ] || continue
        IFS=,
        for _l in $_lbl; do
            IFS="$_ifs_save"
            _l="${_l# }"; _l="${_l% }"
            [ -z "$_l" ] || [ "$_l" = "$_scope" ] && continue
            case " $_part " in *" $_l "*) ;; *) _part="${_part:+$_part }$_l" ;; esac
        done
        IFS="$_ifs_save"
    done

    for id in $ids; do
        [ -z "$id" ] && continue
        n=$((n+1))
        show_out="$(bdq show "$id" --json 2>/dev/null)"
        show_rc=$?
        if [ "$show_rc" -ne 0 ] || [ -z "$show_out" ]; then
            printf 'bead: %s: unreadable (bd show failed)\n' "$id" >&2
            bad=$((bad+1)); rc=1; continue
        fi
        show_parsed="$(printf '%s\n' "$show_out" | python3 -c '
import json, sys
data = json.load(sys.stdin)
d = data[0] if isinstance(data, list) else data
print(" ".join(d.get("labels") or []))
print(d.get("status") or "")
print(d.get("issue_type") or "")
' 2>/dev/null || printf '\n\n')"
        labels="$(printf '%s\n' "$show_parsed" | sed -n '1p')"
        bead_status="$(printf '%s\n' "$show_parsed" | sed -n '2p')"
        bead_type="$(printf '%s\n' "$show_parsed" | sed -n '3p')"
        # A branch: label names ONE bead's own worktree and must never be inherited. `bd
        # create --parent` copies every label from the parent onto a child by default, so a
        # child can carry a branch: that names its parent (or a sibling) instead of itself —
        # the resume preference reads that label first, so a mislabeled bead is claimed
        # ahead of everything else ready, and aeon.sh finds the branch already held by the
        # bead it actually names.
        local _br_lbl="" _br_tok _br_cand
        for _br_tok in $labels; do
            case "$_br_tok" in branch:*) _br_lbl="$_br_tok"; break ;; esac
        done
        if [ -n "$_br_lbl" ]; then
            _br_cand="${_br_lbl#branch:}"; _br_cand="${_br_cand#spira/}"
            if [ -n "$_br_cand" ] && [ "$_br_cand" != "$id" ] && bdq show "$_br_cand" --json >/dev/null 2>&1; then
                printf 'bead: %s: branch: label names %s, not itself\n' "$id" "$_br_cand" >&2
                bad=$((bad+1)); rc=1
            fi
        fi
        # repo: is required only for routable work beads. Non-work kinds (event,
        # escalation, proposal, gate) carry no routing obligation and may omit it.
        case " task bug feature epic chore spike " in
            *" $bead_type "*)
                case " $labels " in
                    *" repo:"*) ;;
                    *) printf 'bead: %s: no repo: label\n' "$id" >&2; bad=$((bad+1)); rc=1 ;;
                esac
                ;;
        esac
        # Open claimable-type beads without a partition label are unclaimable unless
        # marked no-loop. epic and event are excluded from bd ready and need no check.
        if [ "$bead_status" = "open" ]; then
            case " task bug feature chore spike " in
                *" $bead_type "*)
                    local _has_noloop=0
                    [ -n "$_no_loop" ] && case " $labels " in *" $_no_loop "*) _has_noloop=1 ;; esac
                    if [ "$_has_noloop" -eq 0 ]; then
                        local _has=0
                        for _p in $_part; do
                            case " $labels " in *" $_p "*) _has=1; break ;; esac
                        done
                        if [ "$_has" -eq 0 ]; then
                            printf 'bead: %s: no partition label%s\n' "$id" \
                                "${_no_loop:+; add one or mark $_no_loop}" >&2
                            bad=$((bad+1)); rc=1
                        fi
                    fi
                    ;;
            esac
        fi
    done
    [ "$bad" = 0 ] && printf 'bead: %d bead(s) checked, ok\n' "$n"
    return "$rc"
}

_bead_amend() {
    local id="${1:-}"; shift || true
    [ -n "$id" ] || { printf 'bead: amend: id required\n' >&2; return 2; }
    local note="" body_file="" express=""
    while [ $# -gt 0 ]; do
        case "$1" in
            --note)       shift; note="${1:-}" ;;
            --body-file)  shift; body_file="${1:-}" ;;
            --express)    express=1 ;;
            *) printf 'bead: amend: unknown option: %s\n' "$1" >&2; return 2 ;;
        esac
        shift
    done
    [ -n "$note" ] || [ -n "$body_file" ] || [ -n "$express" ] || {
        printf 'bead: amend: --note, --body-file, or --express required\n' >&2; return 2; }

    local changed=""
    if [ -n "$express" ]; then
        local _elab="${SPIRA_EXPRESS_LABEL:-express}"
        bdq label add "$id" "$_elab"
        changed="${changed:+$changed$'\n\n'}Marked express."
    fi
    if [ -n "$note" ]; then
        bdq note "$id" "$note"
        changed="${changed:+$changed$'\n\n'}$note"
    fi
    if [ -n "$body_file" ]; then
        bdq update "$id" --body-file "$body_file"
        changed="${changed:+$changed$'\n\n'}Description updated."
    fi

    # Notify the live aeon if one is working this bead.
    local pf
    for pf in "$SPIRA_RUN"/aeon-*-"$id".pid; do
        [ -f "$pf" ] && aeon_alive "$pf" || continue
        [ -d "${SPIRA_MAIL:-}/aeon-$id/new" ] || break
        SPIRA_MAIL_LINT_CONSIDERED=1 "$BEAD_HOME/mail.sh" send "aeon-$id" \
            --from "amend <amend@spira>" \
            --subject "Update while you work" <<< "$changed" 2>/dev/null || true
        break
    done
}

case "${1:-}" in
    file)     shift; _bead_file "$@" ;;
    amend)    shift; _bead_amend "$@" ;;
    contract) _bead_contract ;;
    lint)     shift; _bead_lint "$@" ;;
    *) printf 'usage: bead.sh file "<title>" --for <persona> --repo <name> [--priority N] [--body-file F] [--express]\n' >&2
       printf '       bead.sh file "<title>" --kind <kind> [--repo <name>] [--priority N] [--body-file F] [--express]\n' >&2
       printf '       bead.sh amend <id> [--note "<text>"] [--body-file F] [--express]\n' >&2
       printf '       bead.sh lint [--all|<id>...]\n' >&2
       printf '       bead.sh contract\n' >&2
       exit 2 ;;
esac
