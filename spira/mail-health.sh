#!/usr/bin/env bash
# mail-health.sh — unread-age check over every registered mailbox.
# Called from watchd.sh notify.
#
# For each mailbox in SPIRA_MAIL_READERS: if mail.sh unread-age exceeds
# SPIRA_MAIL_UNREAD_AGE, mails the operator once per distinct backlog.
# State in SPIRA_RUN/mail-health/<mailbox> — the mtime of the oldest
# unread message when last mailed. Cleared when the mailbox empties so a
# new backlog fires when it recurs.
#
# Exit 0: nothing to report. Exit 1: mailed. Exit 3: could not check.
#
# covers: spira/mail-health.sh spira/mail.sh spira/conf.sh
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

while IFS= read -r _line; do
    case "$_line" in ''|'#'*) continue ;; esac
    _mb="${_line%%=*}"
    _mb="${_mb%"${_mb##*[![:space:]]}"}"
    [ -z "$_mb" ] && continue

    age="$(bash "$_here/mail.sh" unread-age "$_mb" 2>/dev/null)" || { err=1; continue; }

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
    count="$(bash "$_here/mail.sh" count "$_mb" 2>/dev/null || printf '?')"
    age_m=$(( age / 60 ))

    printf '## Note\n%s is not reading its mail (%s message(s), oldest %s minutes unread).\n' \
        "$_mb" "$count" "$age_m" \
    | SPIRA_MAIL_LINT_CONSIDERED="mail-health automated" \
      SPIRA_MAIL_REPEAT_CONSIDERED="mail-health has own dedup" \
      bash "$_here/mail.sh" send operator \
        --from "Mail health <health@spira>" \
        --subject "$_mb is not reading its mail" \
        2>/dev/null || { err=1; continue; }

    printf '%s\n' "$key" > "$sf"
    found=1

done <<< "${SPIRA_MAIL_READERS:-}"

[ "$err" = 1 ] && exit 3
[ "$found" = 1 ] && exit 1
exit 0
