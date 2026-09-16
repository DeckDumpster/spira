#!/usr/bin/env bash
# mail.sh — Maildir mailboxes for operator/concierge messages.
#
#   mail.sh send <mailbox> --from "<s>" --subject "<s>" [--kind K] [--default D] [--bead ID] < body
#   mail.sh list <mailbox> [--unread]
#   mail.sh read <mailbox> [<message>]   prints, moves new -> cur
#   mail.sh unread-age <mailbox>         seconds since oldest unread; empty if none
#   mail.sh sendmail                     RFC 5322 on stdin (separate bead)
#
# Send refuses a message that is missing From, missing Subject, has a Subject
# that is or leads with a bead id, has a body mentioning a bead id without enough
# context to say what the work is, or is a decision/question with no default.
# Each refusal names the rule.  Override: SPIRA_MAIL_LINT_CONSIDERED=1.

set -uo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)/conf.sh"

# Bead id pattern for the configured installation (e.g. sp-[a-z0-9]{4,}).
_bead_id_re="${SPIRA_ID_PREFIX:-sp}-[a-z0-9]{4,}"

_mail_dir()    { printf '%s/%s' "${SPIRA_MAIL}" "$1"; }
_mail_ensure() { local d; d="$(_mail_dir "$1")"; mkdir -p "$d/tmp" "$d/new" "$d/cur"; }
_mail_msgid()  { printf '%s.%s.%s' "$(date +%s)" "$RANDOM" "$$"; }

_lint_check() {
    local from="$1" subject="$2" kind="$3" default="$4" body="$5"
    [ "${SPIRA_MAIL_LINT_CONSIDERED:-}" = "1" ] && return 0
    local fail=0

    if [ -z "$from" ]; then
        printf 'mail: lint: missing From — rule: every message must name a sender\n' >&2
        fail=1
    fi

    if [ -z "$subject" ]; then
        printf 'mail: lint: missing Subject — rule: every message must carry a subject\n' >&2
        fail=1
    else
        if [[ "$subject" =~ ^${_bead_id_re}(:|[[:space:]]|$) ]]; then
            printf 'mail: lint: Subject leads with a bead id — rule: Subject is the human topic; put the id in --bead\n' >&2
            fail=1
        fi
    fi

    # A bead id in the body must be surrounded by enough words to say what the work is.
    if printf '%s' "$body" | grep -qE "${_bead_id_re}"; then
        local line stripped wc_val
        while IFS= read -r line; do
            [[ "$line" =~ ${_bead_id_re} ]] || continue
            stripped="$(printf '%s' "$line" | sed -E "s/${_bead_id_re}//g")"
            wc_val="$(printf '%s' "$stripped" | wc -w)"
            if [ "${wc_val}" -lt 4 ]; then
                printf 'mail: lint: body names a bead id without saying what the work is — rule: describe the work, not just the id\n' >&2
                fail=1
                break
            fi
        done <<< "$body"
    fi

    if [ "$kind" = "decision" ] || [ "$kind" = "question" ]; then
        if [ -z "$default" ]; then
            printf 'mail: lint: %s with no default — rule: decision and question mail must carry X-Spira-Default\n' "$kind" >&2
            fail=1
        fi
    fi

    return "$fail"
}

cmd_send() {
    local mailbox="$1"; shift
    local from="" subject="" kind="" default="" bead=""
    while [ $# -gt 0 ]; do
        case "$1" in
            --from)    from="$2";    shift 2 ;;
            --subject) subject="$2"; shift 2 ;;
            --kind)    kind="$2";    shift 2 ;;
            --default) default="$2"; shift 2 ;;
            --bead)    bead="$2";    shift 2 ;;
            *) printf 'mail.sh send: unknown option: %s\n' "$1" >&2; return 1 ;;
        esac
    done

    local body; body="$(cat)"
    _lint_check "$from" "$subject" "${kind:-}" "${default:-}" "$body" || return 1

    _mail_ensure "$mailbox"
    local dir; dir="$(_mail_dir "$mailbox")"
    local msgid; msgid="$(_mail_msgid)"

    {
        printf 'From: %s\n' "$from"
        printf 'Subject: %s\n' "$subject"
        [ -n "$kind" ]    && printf 'X-Spira-Kind: %s\n' "$kind"
        [ -n "$default" ] && printf 'X-Spira-Default: %s\n' "$default"
        [ -n "$bead" ]    && printf 'X-Spira-Bead: %s\n' "$bead"
        printf 'Date: %s\n' "$(date -u '+%a, %d %b %Y %H:%M:%S +0000')"
        printf '\n'
        printf '%s\n' "$body"
    } > "$dir/tmp/$msgid"

    mv "$dir/tmp/$msgid" "$dir/new/$msgid"
}

cmd_list() {
    local mailbox="$1"; shift
    local unread_only=0
    [ "${1:-}" = "--unread" ] && unread_only=1

    _mail_ensure "$mailbox"
    local dir; dir="$(_mail_dir "$mailbox")"
    local dirs=("$dir/new")
    [ "$unread_only" -eq 0 ] && dirs+=("$dir/cur")

    local f from subject date status
    for d in "${dirs[@]}"; do
        for f in "$d"/*; do
            [ -f "$f" ] || continue
            from="$(sed -n 's/^From:[[:space:]]*//p' "$f" | head -1)"
            subject="$(sed -n 's/^Subject:[[:space:]]*//p' "$f" | head -1)"
            date="$(sed -n 's/^Date:[[:space:]]*//p' "$f" | head -1)"
            [[ "$f" == "$dir/new/"* ]] && status="new" || status="cur"
            printf '%s  [%s]  From: %s  Subject: %s\n' \
                "${date:--}" "$status" "${from:--}" "${subject:--}"
        done
    done
}

cmd_read() {
    local mailbox="$1"; shift
    local msg="${1:-}"
    _mail_ensure "$mailbox"
    local dir; dir="$(_mail_dir "$mailbox")"

    local f=""
    if [ -n "$msg" ]; then
        local candidate
        for candidate in "$dir/new/$msg" "$dir/cur/$msg"; do
            [ -f "$candidate" ] && { f="$candidate"; break; }
        done
        [ -z "$f" ] && { printf 'mail.sh read: %s: not found\n' "$msg" >&2; return 1; }
    else
        local oldest="" candidate
        for candidate in "$dir/new"/*; do
            [ -f "$candidate" ] || continue
            if [ -z "$oldest" ] || [ "$candidate" -ot "$oldest" ]; then
                oldest="$candidate"
            fi
        done
        [ -z "$oldest" ] && { printf 'mail.sh read: no unread mail in %s\n' "$mailbox" >&2; return 1; }
        f="$oldest"
    fi

    cat "$f"
    [[ "$f" == "$dir/new/"* ]] && mv "$f" "$dir/cur/$(basename "$f")"
}

cmd_unread_age() {
    local mailbox="$1"
    _mail_ensure "$mailbox"
    local dir; dir="$(_mail_dir "$mailbox")"

    local oldest_t="" t f
    for f in "$dir/new"/*; do
        [ -f "$f" ] || continue
        t="$(stat -c %Y "$f" 2>/dev/null || stat -f %m "$f" 2>/dev/null)" || continue
        [ -z "$oldest_t" ] || [ "$t" -lt "$oldest_t" ] && oldest_t="$t"
    done

    [ -z "$oldest_t" ] && return 0
    printf '%s\n' "$(( $(date +%s) - oldest_t ))"
}

case "${1:-}" in
    send)       shift; cmd_send "$@" ;;
    list)       shift; cmd_list "$@" ;;
    read)       shift; cmd_read "$@" ;;
    unread-age) shift; cmd_unread_age "$@" ;;
    sendmail)   printf 'mail.sh sendmail: not yet implemented\n' >&2; exit 1 ;;
    *)          printf 'mail.sh: unknown command: %s\n' "${1:-}" >&2; exit 1 ;;
esac
