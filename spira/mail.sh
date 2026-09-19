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

_repeat_check() {
    # Refuses a repeat mail to the same recipient within SPIRA_MAIL_REPEAT_WINDOW.
    # Fingerprint: (caller-script, normalized-subject, recipient). Normalization strips
    # digits and SHA-like hex runs so "requeued 5 times" and "requeued 6 times" are the same
    # escalation. Override: SPIRA_MAIL_REPEAT_CONSIDERED=<reason>, recorded and counted.
    # Each refusal is counted under SPIRA_RUN/mail-repeat so audits are possible.
    local mailbox="$1" subject="$2"
    [ -n "${SPIRA_MAIL_REPEAT_CONSIDERED:-}" ] && return 0

    local caller=""
    local _c0 _c1
    _c0="$(tr '\0' '\n' < "/proc/$PPID/cmdline" 2>/dev/null | sed -n '1p')" || _c0=""
    _c1="$(tr '\0' '\n' < "/proc/$PPID/cmdline" 2>/dev/null | sed -n '2p')" || _c1=""
    caller="$(basename "${_c1:-${_c0:-unknown}}")"

    local norm_subj
    norm_subj="$(printf '%s' "$subject" \
        | sed -E 's/[0-9a-f]{7,}[0-9a-f]*//gI; s/[0-9]+//g' \
        | tr -s ' ')"

    local fp
    fp="$(printf '%s|%s|%s' "$caller" "$norm_subj" "$mailbox" \
        | sha256sum | cut -c1-48)"

    local stamp_dir="${SPIRA_RUN:-/tmp}/mail-repeat"
    local stamp_file="$stamp_dir/$fp"
    local window="${SPIRA_MAIL_REPEAT_WINDOW:-14400}"

    if [ -f "$stamp_file" ]; then
        local stamp_time now elapsed
        stamp_time="$(stat -c %Y "$stamp_file" 2>/dev/null || stat -f %m "$stamp_file" 2>/dev/null)" || stamp_time=0
        now="$(date +%s)"
        elapsed=$(( now - stamp_time ))
        if [ "$elapsed" -lt "$window" ]; then
            mkdir -p "$stamp_dir"
            printf '1\n' >> "$stamp_dir/$fp.refused"
            local nrefused
            nrefused="$(wc -l < "$stamp_dir/$fp.refused" 2>/dev/null | tr -d ' ')"
            printf 'mail: repeat refused — %s already sent to %s within %ss window (refusals: %s) — override: SPIRA_MAIL_REPEAT_CONSIDERED=<reason>\n' \
                "$caller" "$mailbox" "$window" "${nrefused:-1}" >&2
            return 1
        fi
    fi

    mkdir -p "$stamp_dir"
    touch "$stamp_file"
    return 0
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

    # A mail body that promises "the ask below" or "the question below" but contains no
    # ## Question or ## Decision section is lying. The ask must be in the same message or
    # the promise must be removed.
    if printf '%s' "$body" | grep -qiE '\bthe (ask|question|decision) below\b'; then
        if ! printf '%s' "$body" | grep -qE '^## (Question|Decision)'; then
            printf 'mail: lint: body promises an ask below but has no ## Question or ## Decision section — rule: carry the ask in this message or remove the promise\n' >&2
            fail=1
        fi
    fi

    return "$fail"
}

cmd_send() {
    local mailbox="$1"; shift
    # aeon:<id> routes to the aeon's per-claim mailbox; refuse if no live mailbox exists.
    case "$mailbox" in
        aeon:*)
            local _bid="${mailbox#aeon:}"
            mailbox="aeon-$_bid"
            if [ ! -d "$(_mail_dir "$mailbox")/new" ]; then
                printf 'mail: aeon:%s: no live mailbox — bead is not claimed by a live aeon\n' "$_bid" >&2
                return 1
            fi
            ;;
    esac
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
    if [ "$mailbox" = "operator" ]; then
        _repeat_check "$mailbox" "$subject" || return 1
    fi
    _lint_check "$from" "$subject" "${kind:-}" "${default:-}" "${urgent:-}" "$body" || return 1

    _mail_ensure "$mailbox"
    local dir; dir="$(_mail_dir "$mailbox")"
    local msgid; msgid="$(_mail_msgid)"

    # For question/decision kinds with a cited bead and a live database: file a tracking
    # decision bead so the operator's reply closes it — not the cited bead.
    # GUARD: a blocking edge onto a non-decision bead makes work beads unclaimable while
    # law questions wait (sp-aybfy, law-blocking-edges-are-real-dependencies). Refuse it
    # and wire relates_to instead. Override: SPIRA_MAIL_ALLOW_BLOCKING=1 (not in any
    # agent brief; the archivist cannot use it without explicit operator instruction).
    local x_bead="$bead"
    if [ -n "$bead" ] && { [ "$kind" = "question" ] || [ "$kind" = "decision" ]; } \
            && [ -n "${SPIRA_DB:-}" ]; then
        local dec_bead="" _ask_label _cited_type _do_block
        _ask_label="${SPIRA_ASK_LABEL:-needs-operator}"  # literal-ok: bash fallback; SPIRA_ASK_LABEL set by conf.sh
        _cited_type="$("${SPIRA_BD:-bd}" -C "$SPIRA_DB" show "$bead" --json 2>/dev/null \
            | sed -n '/^[[{]/,$p' \
            | python3 -c 'import json,sys; r=json.load(sys.stdin); d=r[0] if isinstance(r,list) else r; print(d.get("issue_type",""))' \
            2>/dev/null)" || _cited_type=""
        if [ "$_cited_type" = "decision" ] || [ -n "${SPIRA_MAIL_ALLOW_BLOCKING:-}" ]; then
            _do_block=1
        else
            _do_block=0
            printf 'mail: blocking edge refused — %s has type %s, not decision; wiring relates_to instead (override: SPIRA_MAIL_ALLOW_BLOCKING=1)\n' \
                "$bead" "${_cited_type:-unknown}" >&2
        fi
        dec_bead="$(printf '%s\n' "$body" \
            | "${SPIRA_BD:-bd}" -C "$SPIRA_DB" create "$subject" \
                -l "$_ask_label,overseer" \
                --type decision \
                --body-file - \
                --silent 2>/dev/null)" || dec_bead=""
        if [ -n "$dec_bead" ]; then
            if [ "$_do_block" -eq 1 ]; then
                "${SPIRA_BD:-bd}" -C "$SPIRA_DB" dep add "$bead" "$dec_bead" >/dev/null 2>&1 || true
            else
                "${SPIRA_BD:-bd}" -C "$SPIRA_DB" dep relate "$dec_bead" "$bead" >/dev/null 2>&1 || true
            fi
        fi
        [ -n "$dec_bead" ] && x_bead="$dec_bead"
    fi

    {
        printf 'From: %s\n' "$from"
        printf 'Subject: %s\n' "$subject"
        [ -n "$kind" ]    && printf 'X-Spira-Kind: %s\n' "$kind"
        [ -n "$default" ] && printf 'X-Spira-Default: %s\n' "$default"
        [ -n "$urgent" ]  && printf 'X-Spira-Urgent: yes\n'
        [ -n "$x_bead" ]  && printf 'X-Spira-Bead: %s\n' "$x_bead"
        [ -n "${SPIRA_MAIL_LINT_CONSIDERED:-}" ] && printf 'X-Spira-Lint-Override: %s\n' "${SPIRA_MAIL_LINT_CONSIDERED}"
        [ -n "${SPIRA_MAIL_REPEAT_CONSIDERED:-}" ] && printf 'X-Spira-Repeat-Override: %s\n' "${SPIRA_MAIL_REPEAT_CONSIDERED}"
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
    # A reply to an aeon persona goes to concierge — the persona is transient and its
    # mailbox would have no reader. Personas are identified by their .md file in chamber/.
    if [ -n "$localpart" ] && [ -f "${SPIRA_HOME:-}/chamber/$localpart.md" ]; then
        printf 'concierge'
        return
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
    # For decision/question kinds: surface the verdict as a note on each blocked work bead.
    if [ "$kind" = "question" ] || [ "$kind" = "decision" ]; then
        local work_ids
        work_ids="$("${SPIRA_BD:-bd}" -C "$SPIRA_DB" dep list "$bead" --direction=up --json 2>/dev/null \
            | python3 -c '
import sys, json
try: d = json.load(sys.stdin)
except Exception: sys.exit(0)
d = d if isinstance(d, list) else [d]
for x in d:
    wid = x.get("id")
    if wid: print(wid)' 2>/dev/null)" || work_ids=""
        local wid
        while IFS= read -r wid; do
            [ -n "$wid" ] || continue
            "${SPIRA_BD:-bd}" -C "$SPIRA_DB" note "$wid" \
                "Operator verdict on decision bead $bead: $reason" >/dev/null 2>&1 || true
        done <<< "$work_ids"
    fi
}

_mark_replied() {
    local f="$1"
    [ -f "$f" ] || return 0
    local dir base dest
    dir="$(dirname "$f")"; base="$(basename "$f")"
    if [[ "$base" == *:2,* ]]; then
        local flags="${base##*:2,}"
        [[ "$flags" == *R* ]] && return 0
        local new_flags; new_flags="$(printf '%sR' "$flags" | fold -w1 | sort | tr -d '\n')"
        dest="$dir/${base%:2,*}:2,$new_flags"
    else
        dest="$dir/${base}:2,R"
    fi
    mv "$f" "$dest" 2>/dev/null || true
}

cmd_done() {
    local mailbox="${1:-}"; shift || true
    local msgid="${1:-}"; shift || true
    local note="${*:-}"
    [ -z "$mailbox" ] && { printf 'mail.sh done: mailbox required\n' >&2; return 1; }
    [ -z "$msgid"   ] && { printf 'mail.sh done: message id required\n' >&2; return 1; }
    _mail_ensure "$mailbox"
    local dir; dir="$(_mail_dir "$mailbox")"
    local f="" candidate
    for candidate in "$dir/new/$msgid" "$dir/cur/$msgid"; do
        [ -f "$candidate" ] && { f="$candidate"; break; }
    done
    if [ -z "$f" ]; then
        for candidate in "$dir/cur/$msgid":*; do
            [ -f "$candidate" ] && { f="$candidate"; break; }
        done
    fi
    [ -z "$f" ] && { printf 'mail.sh done: %s: not found in %s\n' "$msgid" "$mailbox" >&2; return 1; }
    [[ "$f" == "$dir/new/"* ]] && { mv "$f" "$dir/cur/$(basename "$f")"; f="$dir/cur/$(basename "$f")"; }
    local base; base="$(basename "$f")"
    local dest
    if [[ "$base" == *:2,* ]]; then
        local flags="${base##*:2,}"
        if [[ "$flags" != *R* ]]; then
            local new_flags; new_flags="$(printf '%sR' "$flags" | fold -w1 | sort | tr -d '\n')"
            dest="$dir/cur/${base%:2,*}:2,$new_flags"
            mv "$f" "$dest" && f="$dest"
        fi
    else
        dest="$dir/cur/${base}:2,R"
        mv "$f" "$dest" && f="$dest"
    fi
    if [ -n "$note" ]; then
        printf '\n-- done: %s\n%s\n' "$(date -u '+%Y-%m-%d %H:%M UTC')" "$note" >> "$f"
    fi
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
    [ -n "$orig_file" ] && _mark_replied "$orig_file"

    _mail_ensure "$dest_mailbox"
    local dir; dir="$(_mail_dir "$dest_mailbox")"
    local msgid; msgid="$(_mail_msgid)"
    printf '%s\n' "$raw" > "$dir/tmp/$msgid"
    mv "$dir/tmp/$msgid" "$dir/new/$msgid"
}

_is_unread() {
    # new/ messages are always unread. cur/ messages with the S (Seen) flag are read.
    [[ "$1" == */new/* ]] && return 0
    case "$(basename "$1")" in *:2,*S*) return 1 ;; esac
    return 0
}

cmd_tidy() {
    local mailbox="${1:-}"
    [ -z "$mailbox" ] && { printf 'mail.sh tidy: mailbox required\n' >&2; return 1; }
    shift
    local dry_run=0
    while [ $# -gt 0 ]; do
        case "$1" in
            --dry-run) dry_run=1; shift ;;
            *) printf 'mail.sh tidy: unknown option: %s\n' "$1" >&2; return 1 ;;
        esac
    done

    # GUARD: cannot verify ask status without a bead store (law-a-control-that-cannot-check-must-refuse)
    if [ -z "${SPIRA_DB:-}" ]; then
        printf 'tidy: bead store not configured — refusing to move any mail\n' >&2
        return 1
    fi

    local ask_label="$SPIRA_ASK_LABEL"
    local fresh_s="${SPIRA_MAIL_TIDY_FRESH:-86400}"
    local urgent_max_s=604800
    local now; now="$(date +%s)"

    # Collect open ask-labelled bead IDs. Covers all non-closed states.
    local ask_ids ask_rc=0
    ask_ids="$("${SPIRA_BD:-bd}" -C "$SPIRA_DB" list \
        --status open,in_progress,blocked,deferred \
        --label "$ask_label" \
        --limit 0 --brief --json 2>/dev/null \
        | python3 -c '
import json, sys
try:
    d = json.load(sys.stdin)
except Exception:
    sys.exit(1)
if isinstance(d, dict): d = [d]
for x in d:
    i = x.get("id", "")
    if i: print(i)
' 2>/dev/null)" || ask_rc=$?

    if [ "$ask_rc" -ne 0 ]; then
        printf 'tidy: bead store query failed — refusing to move any mail\n' >&2
        return 1
    fi

    # Positive control: an empty result is only trusted if the store responds at all.
    # A broken query returning empty would archive every live ask (law-absence-needs-a-positive-control).
    if [ -z "$ask_ids" ]; then
        local probe_rc=0
        "${SPIRA_BD:-bd}" -C "$SPIRA_DB" list --limit 1 --brief --json >/dev/null 2>&1 \
            || probe_rc=$?
        if [ "$probe_rc" -ne 0 ]; then
            printf 'tidy: positive control failed — bead store unreadable; refusing to move any mail\n' >&2
            return 1
        fi
    fi

    local inbox_dir; inbox_dir="$(_mail_dir "$mailbox")"
    [ -d "$inbox_dir/cur" ] && [ -d "$inbox_dir/new" ] || {
        printf 'tidy: %s: mailbox not found\n' "$mailbox" >&2; return 1
    }

    _mail_ensure "archive"
    local archive_cur; archive_cur="$(_mail_dir archive)/cur"

    # Collect messages sorted by mtime newest first, for dedup processing.
    local sorted_msgs
    sorted_msgs="$(
        local f mt
        for f in "$inbox_dir/new"/* "$inbox_dir/cur"/*; do
            [ -f "$f" ] || continue
            mt="$(stat -c %Y "$f" 2>/dev/null || stat -f %m "$f" 2>/dev/null)" || mt=0
            printf '%s\t%s\n' "$mt" "$f"
        done | sort -rn
    )"

    local seen_subjects="" archived=0 kept=0
    local mtime path subj bead_id keep age urgent_hdr

    while IFS=$'\t' read -r mtime path; do
        [ -f "$path" ] || continue
        subj="$(sed -n 's/^Subject:[[:space:]]*//p' "$path" | head -1)"
        keep=0

        # Dedup: archive older copies of a repeated subject (newest processed first).
        if [ -n "$seen_subjects" ] && printf '%s\n' "$seen_subjects" | grep -qxF "$subj" 2>/dev/null; then
            keep=0
        else
            seen_subjects="${seen_subjects:+${seen_subjects}
}${subj}"

            # Rule 1: carries a bead ID for an open ask-labelled bead.
            bead_id="$(sed -n 's/^X-Spira-Bead:[[:space:]]*//p' "$path" | head -1)"
            if [ -z "$bead_id" ]; then
                bead_id="$(awk '/^$/{body=1;next} body' "$path" \
                    | grep -oE "${_bead_id_re}" | head -1)" || bead_id=""
            fi
            if [ -n "$bead_id" ] && printf '%s\n' "$ask_ids" | grep -qx "$bead_id"; then
                keep=1
            fi

            # Rule 2: unread and younger than the fresh window.
            if [ "$keep" -eq 0 ]; then
                age=$(( now - mtime ))
                if _is_unread "$path" && [ "$age" -lt "$fresh_s" ]; then keep=1; fi
            fi

            # Rule 3: urgent and younger than 7 days.
            if [ "$keep" -eq 0 ]; then
                urgent_hdr="$(sed -n 's/^X-Spira-Urgent:[[:space:]]*//p' "$path" | head -1)"
                age=$(( now - mtime ))
                if [ -n "$urgent_hdr" ] && [ "$age" -lt "$urgent_max_s" ]; then keep=1; fi
            fi
        fi

        if [ "$keep" -eq 1 ]; then
            kept=$(( kept + 1 ))
        else
            archived=$(( archived + 1 ))
            [ "$dry_run" -eq 0 ] && mv "$path" "$archive_cur/$(basename "$path")"
        fi
    done <<< "$sorted_msgs"

    printf 'tidy: archived %d, kept %d\n' "$archived" "$kept"
}

case "${1:-}" in
    send)       shift; cmd_send "$@" ;;
    template)   shift; cmd_template "$@" ;;
    list)       shift; cmd_list "$@" ;;
    read)       shift; cmd_read "$@" ;;
    count)      shift; cmd_count "$@" ;;
    unread-age) shift; cmd_unread_age "$@" ;;
    done)       shift; cmd_done "$@" ;;
    sendmail)   shift; cmd_sendmail "$@" ;;
    tidy)       shift; cmd_tidy "$@" ;;
    *)          printf 'mail.sh: unknown command: %s\n' "${1:-}" >&2; exit 1 ;;
esac
