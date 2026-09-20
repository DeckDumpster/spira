#!/usr/bin/env bash
#
# bead.sh — file a bead through the contract; never call bd create directly.
#
#   bead.sh file "<title>" --for <persona> --repo <name> [--priority N] [--body-file F]
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
# `bead.sh contract` asks the live source — fayths for personas, schema.sh for kinds, the
# repo-map for repos — so the set it prints is exactly the set it can accept.
set -uo pipefail
BEAD_HOME="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$BEAD_HOME/lib.sh"

_bead_file() {
    local title="${1:-}"; shift || true
    [ -n "$title" ] || { printf 'bead: title required\n' >&2; return 2; }
    local for_fayth="" repo="" priority="" body_file=""
    while [ $# -gt 0 ]; do
        case "$1" in
            --for)          shift; for_fayth="${1:-}" ;;
            --repo)         shift; repo="${1:-}" ;;
            --priority|-p)  shift; priority="${1:-}" ;;
            --body-file)    shift; body_file="${1:-}" ;;
            *) printf 'bead: unknown option: %s\n' "$1" >&2; return 2 ;;
        esac
        shift
    done
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
    set -- create "$title" -l "$labels"
    [ -n "$priority" ]  && set -- "$@" -p "$priority"
    [ -n "$body_file" ] && set -- "$@" --body-file "$body_file"
    bdq "$@"
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
    local rc=0 n=0 bad=0 id labels show_out show_rc
    local ids=""
    if [ "${1:-}" = "--all" ] || [ $# -eq 0 ]; then
        ids="$(bdq list --all --limit 0 --json 2>/dev/null \
            | python3 -c '
import json, sys
data = json.load(sys.stdin)
for d in (data if isinstance(data, list) else [data]):
    print(d["id"])
' 2>/dev/null || true)"
    else
        ids="$*"
    fi
    for id in $ids; do
        [ -z "$id" ] && continue
        n=$((n+1))
        show_out="$(bdq show "$id" --json 2>/dev/null)"
        show_rc=$?
        if [ "$show_rc" -ne 0 ] || [ -z "$show_out" ]; then
            printf 'bead: %s: unreadable (bd show failed)\n' "$id" >&2
            bad=$((bad+1)); rc=1; continue
        fi
        labels="$(printf '%s\n' "$show_out" | python3 -c '
import json, sys
data = json.load(sys.stdin)
d = data[0] if isinstance(data, list) else data
print(" ".join(d.get("labels") or []))
' 2>/dev/null || true)"
        case " $labels " in
            *" repo:"*) ;;
            *) printf 'bead: %s: no repo: label\n' "$id" >&2; bad=$((bad+1)); rc=1 ;;
        esac
    done
    [ "$bad" = 0 ] && printf 'bead: %d bead(s) checked, ok\n' "$n"
    return "$rc"
}

_bead_amend() {
    local id="${1:-}"; shift || true
    [ -n "$id" ] || { printf 'bead: amend: id required\n' >&2; return 2; }
    local note="" body_file=""
    while [ $# -gt 0 ]; do
        case "$1" in
            --note)       shift; note="${1:-}" ;;
            --body-file)  shift; body_file="${1:-}" ;;
            *) printf 'bead: amend: unknown option: %s\n' "$1" >&2; return 2 ;;
        esac
        shift
    done
    [ -n "$note" ] || [ -n "$body_file" ] || {
        printf 'bead: amend: --note or --body-file required\n' >&2; return 2; }

    local changed=""
    if [ -n "$note" ]; then
        bdq note "$id" "$note"
        changed="$note"
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
    *) printf 'usage: bead.sh file "<title>" --for <persona> --repo <name> [--priority N] [--body-file F]\n' >&2
       printf '       bead.sh amend <id> [--note "<text>"] [--body-file F]\n' >&2
       printf '       bead.sh lint [--all|<id>...]\n' >&2
       printf '       bead.sh contract\n' >&2
       exit 2 ;;
esac
