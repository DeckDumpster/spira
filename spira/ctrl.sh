#!/usr/bin/env bash
# ctrl.sh — operational control plane: durable state outside source control.
#
#   ctrl.sh suspend <subject> --reason <text> --owner <bead>
#   ctrl.sh resume  <subject>
#   ctrl.sh check   <subject>          exits 0 if suspended, 1 if not
#   ctrl.sh list                       print all entries
#   ctrl.sh divergence                 declared-suspended but active/enabled in systemd
#
# WHAT THE CONTROL PLANE IS FOR. A control operation — suspending a timer, pausing a
# persona, draining a lane — is not a source-code change. The prior art was editing
# a units list and committing it, which placed the decision inside the thing that executes
# the decision: a source edit is reverted by pull, reset, and install.sh. This tool stores
# state at $SPIRA_CTRL, which lives in the gitignored runtime directory and is never written
# by git, install.sh, or any other source-management operation.
#
# EVERY ENTRY CARRIES A REASON AND AN OWNING BEAD. An entry without either is refused at
# write time. Without a reason, "why is this not running" has no answer inside the same
# file that says it is not running. Without an owner, the suspension becomes permanent by
# default — nothing has declared responsibility for lifting it.
#
# DIVERGENCE. This tool reports; it does not reconcile. A declared suspension found running
# anyway is a control decision the system overrode; it does not re-apply automatically,
# because a loop that re-applies intent hides the same class of fault in the other direction
# — the automated version of the manual stop-disable-mask cycle that was erased by every
# install before this mechanism existed.
#
# STORAGE FORMAT. One JSON file at $SPIRA_CTRL. Flat object keyed by subject name; each
# value is an object keyed by operation type; each leaf carries reason, owner, and when.
# New operation types extend the file without changing the structure.
#
#   {
#     "spira-suites": {
#       "suspend": {
#         "reason": "...",
#         "owner": "sp-xxxx",
#         "when":  "2026-09-12",
#         "by":    "operator"
#       }
#     }
#   }
set -uo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)/conf.sh"

# systemctl behind a seam so test suites can stub it without reaching the box.
SC="${SPIRA_SYSTEMCTL:-systemctl}"

_usage() {
    printf 'ctrl.sh — operational control plane\n\n' >&2
    printf '  ctrl.sh suspend <subject> --reason <text> --owner <bead>\n' >&2
    printf '  ctrl.sh resume  <subject>\n' >&2
    printf '  ctrl.sh check   <subject>\n' >&2
    printf '  ctrl.sh list\n' >&2
    printf '  ctrl.sh divergence\n' >&2
    exit 1
}

# _read_ctrl -> prints the JSON content, or '{}' if the file does not exist.
_read_ctrl() {
    if [ -f "${SPIRA_CTRL}" ]; then cat "${SPIRA_CTRL}"; else printf '{}'; fi
}

# _write_ctrl <json> -> atomically write the JSON to $SPIRA_CTRL.
_write_ctrl() {
    mkdir -p "$(dirname "${SPIRA_CTRL}")"
    local tmp
    tmp="$(mktemp "${SPIRA_CTRL}.XXXXXX")"
    printf '%s\n' "$1" > "$tmp"
    mv "$tmp" "${SPIRA_CTRL}"
}

do_suspend() {
    local subject="${1:-}"
    [ -n "$subject" ] || { printf 'ctrl: suspend requires a subject\n' >&2; exit 1; }
    shift
    local reason="" owner="" by="${USER:-operator}"
    while [ $# -gt 0 ]; do
        case "$1" in
            --reason) reason="${2:-}"; shift 2 ;;
            --owner)  owner="${2:-}";  shift 2 ;;
            *)        printf 'ctrl: unknown flag: %s\n' "$1" >&2; exit 1 ;;
        esac
    done
    [ -n "$reason" ] || { printf 'ctrl: --reason is required\n' >&2; exit 1; }
    [ -n "$owner"  ] || { printf 'ctrl: --owner is required\n'  >&2; exit 1; }

    local when
    when="$(TZ="${SPIRA_TZ:-}" date '+%Y-%m-%d' 2>/dev/null || date '+%Y-%m-%d')"

    local new_json
    new_json="$(python3 - "$(_read_ctrl)" "$subject" "$reason" "$owner" "$when" "$by" <<'PY'
import json, sys
data = json.loads(sys.argv[1])
subject, reason, owner, when, by = sys.argv[2:]
if subject not in data:
    data[subject] = {}
data[subject]['suspend'] = {
    'reason': reason,
    'owner':  owner,
    'when':   when,
    'by':     by,
}
print(json.dumps(data, indent=2, sort_keys=True))
PY
)" || { printf 'ctrl: failed to update control file\n' >&2; exit 1; }

    _write_ctrl "$new_json"
    printf 'ctrl: suspended %s (owner: %s)\n' "$subject" "$owner"
}

do_resume() {
    local subject="${1:-}"
    [ -n "$subject" ] || { printf 'ctrl: resume requires a subject\n' >&2; exit 1; }

    if [ ! -f "${SPIRA_CTRL}" ]; then
        printf 'ctrl: %s is not suspended (no control file)\n' "$subject"
        return 0
    fi

    # Python prints the new JSON on line 1..N-1 and a status word on the last line.
    local result new_json status_line
    result="$(python3 - "$(_read_ctrl)" "$subject" <<'PY'
import json, sys
data = json.loads(sys.argv[1])
subject = sys.argv[2]
was = subject in data and 'suspend' in data[subject]
if was:
    del data[subject]['suspend']
    if not data[subject]:
        del data[subject]
print(json.dumps(data, indent=2, sort_keys=True))
print("WAS_SUSPENDED" if was else "NOT_SUSPENDED")
PY
)" || { printf 'ctrl: failed to update control file\n' >&2; exit 1; }

    status_line="$(printf '%s\n' "$result" | tail -n 1)"
    new_json="$(printf '%s\n' "$result" | head -n -1)"

    _write_ctrl "$new_json"
    case "$status_line" in
        WAS_SUSPENDED) printf 'ctrl: resumed %s\n' "$subject" ;;
        *)             printf 'ctrl: %s was not suspended\n' "$subject" ;;
    esac
}

# check <subject> -> exits 0 if suspended, 1 if not (silent).
do_check() {
    local subject="${1:-}"
    [ -n "$subject" ] || { printf 'ctrl: check requires a subject\n' >&2; exit 1; }
    [ -f "${SPIRA_CTRL}" ] || return 1
    python3 - "${SPIRA_CTRL}" "$subject" <<'PY'
import json, sys
try:
    with open(sys.argv[1]) as f:
        data = json.load(f)
    subject = sys.argv[2]
    sys.exit(0 if (subject in data and 'suspend' in data[subject]) else 1)
except Exception:
    sys.exit(1)
PY
}

do_list() {
    if [ ! -f "${SPIRA_CTRL}" ]; then
        printf 'ctrl: no control file — no entries\n'
        return 0
    fi
    python3 - "${SPIRA_CTRL}" <<'PY'
import json, sys
try:
    with open(sys.argv[1]) as f:
        data = json.load(f)
except Exception as e:
    print(f"ctrl: cannot read control file: {e}", file=sys.stderr)
    sys.exit(1)
if not data:
    print("ctrl: no entries")
    sys.exit(0)
for subject, ops in sorted(data.items()):
    for op, entry in sorted(ops.items()):
        print(f"  {subject}  [{op}]  owner:{entry.get('owner','?')}  "
              f"since:{entry.get('when','?')}  by:{entry.get('by','?')}")
        print(f"    reason: {entry.get('reason','?')}")
PY
}

# divergence -> for each declared suspension, check whether the unit is actually
# active or enabled in systemd. Exits 1 if any divergence is found, 0 if clean.
#
# A POSITIVE CONTROL IS BUILT IN. When no suspensions are declared, this fact is printed
# explicitly rather than silently. An empty report that gives no evidence it ran is not a
# clean verdict; a positive report from an empty set is (law-absence-needs-a-positive-control).
do_divergence() {
    if [ ! -f "${SPIRA_CTRL}" ]; then
        printf 'ctrl: no control file — no suspensions declared\n'
        return 0
    fi

    local suspended_subjects
    suspended_subjects="$(python3 - "${SPIRA_CTRL}" <<'PY'
import json, sys
try:
    with open(sys.argv[1]) as f:
        data = json.load(f)
    for s, ops in sorted(data.items()):
        if 'suspend' in ops:
            print(s)
except Exception as e:
    print(f"ctrl: {e}", file=sys.stderr)
    sys.exit(2)
PY
)" || return 2

    if [ -z "$suspended_subjects" ]; then
        printf 'ctrl: no suspensions declared\n'
        return 0
    fi

    local inst="${SPIRA_INSTANCE:-prod}"
    local div_found=0 subject unit state enabled

    while IFS= read -r subject; do
        [ -n "$subject" ] || continue
        # Check instance-qualified forms first, then plain forms. A subject like
        # "spira-suites" corresponds to "spira-suites-prod.timer" on instance "prod".
        # Non-spira units (e.g. "cockpit-ensure") keep their plain names.
        for unit in "${subject}-${inst}.timer"  "${subject}-${inst}.service" \
                    "${subject}.timer"           "${subject}.service"; do
            state="$("$SC" --user is-active   "$unit" 2>/dev/null || true)"
            enabled="$("$SC" --user is-enabled "$unit" 2>/dev/null || true)"
            if [ "$state" = "active" ] || [ "$enabled" = "enabled" ]; then
                printf 'ctrl: DIVERGENCE %s declared suspended but %s is %s (enabled: %s)\n' \
                    "$subject" "$unit" "${state:-inactive}" "${enabled:-?}"
                div_found=1
            fi
        done
    done <<< "$suspended_subjects"

    [ "$div_found" = 1 ] && return 1
    return 0
}

[ $# -ge 1 ] || _usage
cmd="$1"; shift
case "$cmd" in
    suspend)    do_suspend "$@" ;;
    resume)     do_resume  "$@" ;;
    check)      do_check   "$@" ;;
    list)       do_list ;;
    divergence) do_divergence ;;
    *)          _usage ;;
esac
