#!/usr/bin/env bash
# mail-health.sh — unread-age check over every registered mailbox, and the operator's reply path.
# Called from watchd notify.
#
# For each mailbox in SPIRA_MAIL_READERS: if mail unread-age exceeds
# SPIRA_MAIL_UNREAD_AGE, mails the operator once per distinct backlog.
# State in SPIRA_RUN/mail-health/<mailbox> — the mtime of the oldest
# unread message when last mailed. Cleared when the mailbox empties so a
# new backlog fires when it recurs.
#
# Outbound: the `outgoing` command in aerc's accounts.conf must be an executable (and, for
# spira-sendmail, pass its own --check); otherwise the operator can read mail but not answer
# it, and is mailed once per distinct fault. No accounts.conf means no aerc, so nothing to check.
#
# Exit 0: nothing to report. Exit 1: mailed. Exit 3: could not check.
#
# covers: spira/mail-health.sh mail/src/* spira/conf.sh
set -uo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)/conf.sh"

_here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
_state_dir="${SPIRA_RUN}/mail-health"

_oldest_mtime() {
    local dir="${SPIRA_MAIL}/$1/new"
    local oldest="" t f
    for f in "$dir"/*; do
        [ -f "$f" ] || continue
        t="$(stat -c %Y "$f" 2>/dev/null || stat -f %m "$f" 2>/dev/null)" || continue
        { [ -z "$oldest" ] || [ "$t" -lt "$oldest" ]; } && oldest="$t"
    done
    printf '%s' "${oldest:-}"
}

found=0; err=0

_aerc_outgoing() {
    local conf="${XDG_CONFIG_HOME:-$HOME/.config}/aerc/accounts.conf" l
    [ -f "$conf" ] || return 1
    while IFS= read -r l; do
        case "$l" in
            outgoing*=*) l="${l#*=}"; l="${l#"${l%%[![:space:]]*}"}"; printf '%s' "${l%"${l##*[![:space:]]}"}"; return 0 ;;
        esac
    done < "$conf"
    return 1
}

_outbound_fault() {
    local cmd="$1" exe
    [ -n "$cmd" ] || { printf 'aerc has an empty outgoing command'; return; }
    exe="${cmd%% *}"
    [ -x "$exe" ] && [ -f "$exe" ] || { printf 'aerc outgoing command %s is not an executable file' "$exe"; return; }
    case "${exe##*/}" in
        spira-sendmail) "$exe" --check >/dev/null 2>&1 || printf '%s --check failed' "$exe" ;;
    esac
}

if _out="$(_aerc_outgoing)"; then
    fault="$(_outbound_fault "$_out")"
    _osf="${_state_dir}/outbound"
    if [ -z "$fault" ]; then
        rm -f "$_osf" 2>/dev/null
    else
        prev=""; [ -r "$_osf" ] && IFS= read -r prev < "$_osf" 2>/dev/null
        if [ "$prev" != "$fault" ]; then
            mkdir -p "$_state_dir" 2>/dev/null
            printf '## Note\nYou cannot reply to mail: %s. Re-run spira-install to rewrite aerc'"'"'s outgoing command.\n' "$fault" \
            | SPIRA_MAIL_REPEAT_CONSIDERED="mail-health has own dedup" \
              mail send operator \
                --from "Mail health <health@spira>" \
                --subject "reply path is broken: aerc cannot send" \
                && { printf '%s\n' "$fault" > "$_osf"; found=1; } \
                || { echo "mail-health: could not mail the operator about the reply path" >&2; err=1; }
        fi
    fi
fi

while IFS= read -r _line; do
    case "$_line" in ''|'#'*) continue ;; esac
    _mb="${_line%%=*}"
    _mb="${_mb%"${_mb##*[![:space:]]}"}"
    [ -z "$_mb" ] && continue

    age="$(mail unread-age "$_mb")" || { echo "mail-health: mail unread-age $_mb failed" >&2; err=1; continue; }  # stderr reaches the journal: "could not check" must say why (sp-xp0u2)

    sf="${_state_dir}/${_mb}"

    if [ -z "$age" ]; then
        rm -f "$sf" 2>/dev/null
        continue
    fi

    case "$age" in ''|*[!0-9]*) err=1; continue ;; esac
    [ "$age" -ge "${SPIRA_MAIL_UNREAD_AGE}" ] || continue

    key="$(_oldest_mtime "$_mb")"
    [ -z "$key" ] && continue

    prev=""
    [ -r "$sf" ] && IFS= read -r prev < "$sf" 2>/dev/null
    [ "$prev" = "$key" ] && continue

    mkdir -p "$_state_dir" 2>/dev/null
    count="$(mail count "$_mb" 2>/dev/null || printf '?')"
    age_m=$(( age / 60 ))

    printf '## Note\n%s is not reading its mail (%s message(s), oldest %s minutes unread).\n' \
        "$_mb" "$count" "$age_m" \
    | SPIRA_MAIL_REPEAT_CONSIDERED="mail-health has own dedup" \
      mail send operator \
        --from "Mail health <health@spira>" \
        --subject "$_mb is not reading its mail" \
        || { echo "mail-health: could not mail the operator about $_mb (see mail's error above)" >&2; err=1; continue; }

    printf '%s\n' "$key" > "$sf"
    found=1

done <<< "${SPIRA_MAIL_READERS:-}"

[ "$err" = 1 ] && exit 3
[ "$found" = 1 ] && exit 1
exit 0
