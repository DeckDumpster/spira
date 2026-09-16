#!/usr/bin/env bash
# mail.sh — Maildir mailboxes for operator/concierge messages.
#
#   mail.sh send <mailbox> --from "<s>" --subject "<s>" [--kind K] [--urgent]
#                          [--default D] [--bead ID] < body
#   mail.sh template <kind>              print the kind's body skeleton
#   mail.sh list <mailbox> [--unread]
#   mail.sh read <mailbox> [<message>]   prints, moves new -> cur
#   mail.sh count <mailbox>              number of unread messages
#   mail.sh unread-age <mailbox>         seconds since oldest unread; empty if none
#   mail.sh sendmail                     RFC 5322 on stdin; closes tracking bead on reply
#
# Send refuses a message that is missing From, missing Subject, has a Subject
# that is or leads with a bead id, has a body mentioning a bead id without enough
# context to say what the work is, has an unknown kind, is missing a header the
# kind requires, has an empty required section, or is urgent without
# "## Why it is urgent".  Each refusal names the rule.
# Override: SPIRA_MAIL_LINT_CONSIDERED=<reason>, recorded in X-Spira-Lint-Override.

set -uo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)/conf.sh"

_bead_id_re="${SPIRA_ID_PREFIX:-sp}-[a-z0-9]{4,}"

_mail_dir()    { printf '%s/%s' "${SPIRA_MAIL}" "$1"; }
_mail_ensure() { local d; d="$(_mail_dir "$1")"; mkdir -p "$d/tmp" "$d/new" "$d/cur"; }
_mail_msgid()  { printf '%s.%s.%s' "$(date +%s)" "$RANDOM" "$$"; }

_kind_file()    { printf '%s/%s.md' "${SPIRA_MAIL_KINDS}" "$1"; }
_kind_exists()  { [ -f "$(_kind_file "$1")" ]; }

_kind_requires() {
    awk '/^---$/ { delim++; next }
         delim == 1 && /^requires:/ { line=$0; sub(/^requires:[[:space:]]*/, "", line); print line }
         delim >= 2 { exit }' "$(_kind_file "$1")"
}

_kind_sections() {
    awk '/^---$/ { delim++; next }
         delim >= 2 && /^## / { print substr($0, 4) }' "$(_kind_file "$1")"
}

_kind_template() {
    awk '/^---$/ { delim++; next } delim >= 2' "$(_kind_file "$1")"
}

_section_empty() {
    # Returns 0 (true) if section is missing or has no non-whitespace content.
    local section="$1" body="$2"
    local result
    result="$(printf '%s\n' "$body" | awk -v sec="## $section" '
        $0 == sec { found=1; next }
        found && /^##/ { exit }
        found && /[^[:space:]]/ { print "x"; exit }
    ')"
    [ -z "$result" ]
}

_lint_check() {
    local from="$1" subject="$2" kind="$3" default="$4" urgent="$5" body="$6"
    [ -n "${SPIRA_MAIL_LINT_CONSIDERED:-}" ] && return 0
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

    if printf '%s' "$body" | grep -qE "${_bead_id_re}"; then
        local line stripped wc_val
        while IFS= read -r line; do
            [[ "$line" =~ ${_bead_id_re} ]] || continue
            # A metadata key-value line (^key: ...) names a bead id in context.
            [[ "$line" =~ ^[a-z_-]+:[[:space:]] ]] && continue
            stripped="$(printf '%s' "$line" | sed -E "s/${_bead_id_re}//g")"
            wc_val="$(printf '%s' "$stripped" | wc -w)"
            if [ "${wc_val}" -lt 4 ]; then
                printf 'mail: lint: body names a bead id without saying what the work is — rule: describe the work, not just the id\n' >&2
                fail=1
                break
            fi
        done <<< "$body"
    fi

    if [ -n "$kind" ]; then
        if ! _kind_exists "$kind"; then
            printf 'mail: lint: unknown kind %s — rule: kind must be a file in %s\n' "$kind" "${SPIRA_MAIL_KINDS}" >&2
            fail=1
        else
            local req h
            req="$(_kind_requires "$kind")"
            for h in $req; do
                case "$h" in
                    X-Spira-Default)
                        [ -z "$default" ] && {
                            printf 'mail: lint: kind %s requires X-Spira-Default — rule: supply --default\n' "$kind" >&2
                            fail=1
                        } ;;
                    X-Spira-Urgent)
                        [ -z "$urgent" ] && {
                            printf 'mail: lint: kind %s requires X-Spira-Urgent — rule: supply --urgent\n' "$kind" >&2
                            fail=1
                        } ;;
                esac
            done

            local section
            while IFS= read -r section; do
                [ -z "$section" ] && continue
                if _section_empty "$section" "$body"; then
                    printf 'mail: lint: section "%s" is empty — rule: every %s section must be filled\n' "$section" "$kind" >&2
                    fail=1
                fi
            done < <(_kind_sections "$kind")
        fi
    fi

    if [ -n "$urgent" ]; then
        if _section_empty "Why it is urgent" "$body"; then
            printf 'mail: lint: urgent message missing "## Why it is urgent" — rule: urgent messages must explain urgency\n' >&2
            fail=1
        fi
    fi

    return "$fail"
}

cmd_send() {
    local mailbox="$1"; shift
    local from="" subject="" kind="" default="" bead="" urgent=""
    while [ $# -gt 0 ]; do
        case "$1" in
            --from)    from="$2";    shift 2 ;;
            --subject) subject="$2"; shift 2 ;;
            --kind)    kind="$2";    shift 2 ;;
            --default) default="$2"; shift 2 ;;
            --bead)    bead="$2";    shift 2 ;;
            --urgent)  urgent=1;     shift ;;
            *) printf 'mail.sh send: unknown option: %s\n' "$1" >&2; return 1 ;;
        esac
    done

    local body; body="$(cat)"
    _lint_check "$from" "$subject" "${kind:-}" "${default:-}" "${urgent:-}" "$body" || return 1

    _mail_ensure "$mailbox"
    local dir; dir="$(_mail_dir "$mailbox")"
    local msgid; msgid="$(_mail_msgid)"

    {
        printf 'From: %s\n' "$from"
        printf 'Subject: %s\n' "$subject"
        [ -n "$kind" ]    && printf 'X-Spira-Kind: %s\n' "$kind"
        [ -n "$default" ] && printf 'X-Spira-Default: %s\n' "$default"
        [ -n "$urgent" ]  && printf 'X-Spira-Urgent: yes\n'
        [ -n "$bead" ]    && printf 'X-Spira-Bead: %s\n' "$bead"
        [ -n "${SPIRA_MAIL_LINT_CONSIDERED:-}" ] && printf 'X-Spira-Lint-Override: %s\n' "${SPIRA_MAIL_LINT_CONSIDERED}"
        printf 'Date: %s\n' "$(date -u '+%a, %d %b %Y %H:%M:%S +0000')"
        printf 'Message-ID: <%s@spira>\n' "$msgid"
        printf '\n'
        printf '%s\n' "$body"
    } > "$dir/tmp/$msgid"

    mv "$dir/tmp/$msgid" "$dir/new/$msgid"
}

cmd_template() {
    local kind="${1:-}"
    [ -z "$kind" ] && { printf 'mail.sh template: kind required\n' >&2; return 1; }
    if ! _kind_exists "$kind"; then
        printf 'mail.sh template: unknown kind: %s\n' "$kind" >&2
        return 1
    fi
    _kind_template "$kind"
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

cmd_count() {
    local mailbox="$1"
    _mail_ensure "$mailbox"
    local dir; dir="$(_mail_dir "$mailbox")"
    local count=0 f
    for f in "$dir/new"/*; do
        [ -f "$f" ] && count=$((count+1))
    done
    printf '%d\n' "$count"
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

_find_message_by_id() {
    local msgid="${1#<}"; msgid="${msgid%>}"
    [ -z "$msgid" ] && return 1
    local d f fid
    for d in "$SPIRA_MAIL"/*/new "$SPIRA_MAIL"/*/cur; do
        [ -d "$d" ] || continue
        for f in "$d"/*; do
            [ -f "$f" ] || continue
            fid="$(awk '/^[[:space:]]*$/ { exit }
                tolower($0) ~ /^message-id:/ { sub(/^[^:]*:[[:space:]]*/, ""); gsub(/[<>]/, ""); print; exit }
            ' "$f")"
            [ "$fid" = "$msgid" ] && { printf '%s' "$f"; return 0; }
        done
    done
    return 1
}

_reply_mailbox() {
    local from="$1" localpart=""
    if [[ "$from" =~ \<([^@>]+)@ ]]; then
        localpart="${BASH_REMATCH[1]}"
    elif [[ "$from" =~ ^([^@[:space:]]+)@ ]]; then
        localpart="${BASH_REMATCH[1]}"
    fi
    if [ -n "$localpart" ] && [ -d "$(_mail_dir "$localpart")" ]; then
        printf '%s' "$localpart"
    else
        printf 'concierge'
    fi
}

_sendmail_close_bead() {
    local bead="$1" kind="$2" first_para="$3"
    [ -n "${SPIRA_DB:-}" ] || return 0
    local reason="$first_para"
    if [ "$kind" = "suit" ]; then
        local lc="${first_para,,}"
        if [[ "$lc" =~ ^uphold ]]; then
            reason="upheld"
        elif [[ "$lc" =~ ^retire ]]; then
            reason="retired"
        elif [[ "$lc" =~ ^amend ]]; then
            reason="$(printf '%s' "$first_para" | sed 's/^[Aa][Mm][Ee][Nn][Dd][: ]*//')"
            [ -z "$reason" ] && reason="amended"
        fi
    fi
    "${SPIRA_BD:-bd}" -C "$SPIRA_DB" close "$bead" --reason-file - <<< "$reason" >/dev/null 2>&1 || true
}

cmd_sendmail() {
    local raw; raw="$(cat)"

    local in_reply_to
    in_reply_to="$(printf '%s\n' "$raw" | awk '
        /^[[:space:]]*$/ { exit }
        tolower($0) ~ /^in-reply-to:/ { sub(/^[^:]*:[[:space:]]*/, ""); gsub(/[<>]/, ""); print; exit }
    ')"

    local first_para
    first_para="$(printf '%s\n' "$raw" | awk '
        /^[[:space:]]*$/ { if (!body) body=1; else if (found) exit; next }
        body { found=1; print }
    ')"

    local orig_file="" orig_bead="" orig_kind="" orig_from="" dest_mailbox="concierge"

    if [ -n "$in_reply_to" ]; then
        orig_file="$(_find_message_by_id "$in_reply_to")" || orig_file=""
        if [ -n "$orig_file" ]; then
            orig_bead="$(awk '/^[[:space:]]*$/ { exit }
                tolower($0) ~ /^x-spira-bead:/ { sub(/^[^:]*:[[:space:]]*/, ""); print; exit }
            ' "$orig_file")"
            orig_kind="$(awk '/^[[:space:]]*$/ { exit }
                tolower($0) ~ /^x-spira-kind:/ { sub(/^[^:]*:[[:space:]]*/, ""); print; exit }
            ' "$orig_file")"
            orig_from="$(awk '/^[[:space:]]*$/ { exit }
                tolower($0) ~ /^from:/ { sub(/^[^:]*:[[:space:]]*/, ""); print; exit }
            ' "$orig_file")"
            dest_mailbox="$(_reply_mailbox "$orig_from")"
        fi
    fi

    [ -n "${orig_bead:-}" ] && _sendmail_close_bead "$orig_bead" "${orig_kind:-}" "$first_para"

    _mail_ensure "$dest_mailbox"
    local dir; dir="$(_mail_dir "$dest_mailbox")"
    local msgid; msgid="$(_mail_msgid)"
    printf '%s\n' "$raw" > "$dir/tmp/$msgid"
    mv "$dir/tmp/$msgid" "$dir/new/$msgid"
}

case "${1:-}" in
    send)       shift; cmd_send "$@" ;;
    template)   shift; cmd_template "$@" ;;
    list)       shift; cmd_list "$@" ;;
    read)       shift; cmd_read "$@" ;;
    count)      shift; cmd_count "$@" ;;
    unread-age) shift; cmd_unread_age "$@" ;;
    sendmail)   shift; cmd_sendmail "$@" ;;
    *)          printf 'mail.sh: unknown command: %s\n' "${1:-}" >&2; exit 1 ;;
esac
