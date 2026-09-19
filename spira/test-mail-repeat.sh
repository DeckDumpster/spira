#!/usr/bin/env bash
#
# test-mail-repeat.sh — mail.sh repeat guard: a second send with the same
# (normalized) subject to operator within the window is refused; a genuinely
# different subject from the same sender still gets through.
#
# POSITIVE CONTROL FIRST (law-absence-needs-a-positive-control): the guard
# MUST fire on a repeat before any negative control runs. Silence from the
# negative controls is evidence the guard works, not that it never could.
#
# covers: spira/mail.sh spira/conf.sh
# shellcheck disable=SC2034
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
isz()    { [ "$2" = 0 ] && ok "$1" || bad "$1" "wanted exit 0 got $2"; }
isnz()   { [ "$2" != 0 ] && ok "$1" || bad "$1" "wanted non-zero exit got 0"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

echo "test-mail-repeat.sh"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM

export SPIRA_MAIL="$TMP/mail"
export SPIRA_RUN="$TMP/run"
export SPIRA_CONF=""
export SPIRA_ID_PREFIX="sp"
export SPIRA_MAIL_REPEAT_WINDOW=3600

run() { bash "$HERE/mail.sh" "$@"; }

body() { printf 'Test body for %s.\n' "$1"; }

# ==========================================================================
# POSITIVE CONTROL — second send with same normalized subject is refused
# ==========================================================================
echo
echo "positive control — repeat to operator is refused"

body first | run send operator --from "Sentinel <sentinel@spira>" \
    --subject "Spira bead sp-abc — requeued 5 times, never landed — harness cannot land it" \
    --kind question --default "close or fix" >/dev/null 2>&1
rc_first=$?
isz "first send exits 0" "$rc_first"

out="$(body second | run send operator --from "Sentinel <sentinel@spira>" \
    --subject "Spira bead sp-abc — requeued 6 times, never landed — harness cannot land it" \
    --kind question --default "close or fix" 2>&1)"
rc_second=$?
isnz "second send (count changed, same normalized subject) is refused" "$rc_second"
want "refusal message names the override" "SPIRA_MAIL_REPEAT_CONSIDERED" "$out"
want "refusal message names the caller" "sentinel" "$out"

# ==========================================================================
# POSITIVE CONTROL — refusal is counted
# ==========================================================================
echo
echo "refusal is counted under SPIRA_RUN/mail-repeat"

refused_count="$(find "$TMP/run/mail-repeat" -name "*.refused" -exec wc -l {} + 2>/dev/null \
    | awk '/total/ { print $1 } NR==1 && !/total/ { print $1 }' | head -1)"
isnz "at least one refusal was recorded" "${refused_count:-0}"

# ==========================================================================
# NEGATIVE CONTROL — a genuinely different subject from same sender gets through
# ==========================================================================
echo
echo "negative control — different subject from same sender gets through"

out="$(body other | run send operator --from "Sentinel <sentinel@spira>" \
    --subject "Spira bead sp-xyz — poisoned after 3 attempts — change the approach or drop it?" \
    --kind question --default "close or relabel" 2>&1)"
rc_other=$?
isz "different subject (different bead, different verb) gets through" "$rc_other"
nowant "different subject carries no repeat-refused message" "repeat refused" "$out"

# ==========================================================================
# OVERRIDE — SPIRA_MAIL_REPEAT_CONSIDERED bypasses the guard
# ==========================================================================
echo
echo "override — SPIRA_MAIL_REPEAT_CONSIDERED bypasses the guard"

out="$(SPIRA_MAIL_REPEAT_CONSIDERED="testing override" \
    body override | run send operator --from "Sentinel <sentinel@spira>" \
    --subject "Spira bead sp-abc — requeued 7 times, never landed — harness cannot land it" \
    --kind question --default "close or fix" 2>&1)"
rc_override=$?
isz "SPIRA_MAIL_REPEAT_CONSIDERED lets the repeat through" "$rc_override"

# Check the override is recorded in the delivered message header.
msg_file="$(ls "$SPIRA_MAIL/operator/new/" 2>/dev/null | tail -1)"
if [ -n "$msg_file" ]; then
    msg_content="$(cat "$SPIRA_MAIL/operator/new/$msg_file" 2>/dev/null)"
    want "override reason recorded in X-Spira-Repeat-Override header" \
         "X-Spira-Repeat-Override: testing override" "$msg_content"
fi

# ==========================================================================
# NON-OPERATOR — repeat guard does not apply to other mailboxes
# ==========================================================================
echo
echo "non-operator mailbox — repeat guard does not apply"

body nc1 | run send concierge --from "Builder <builder@spira>" \
    --subject "Build complete for sp-abc" >/dev/null 2>&1 || true
rc_nc="$(body nc2 | run send concierge --from "Builder <builder@spira>" \
    --subject "Build complete for sp-abc" 2>/dev/null; echo $?)"
is "concierge mailbox allows repeat" "0" "$rc_nc"

echo
printf '  %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
