#!/usr/bin/env bash
# tier: T2
# covers: aerc/accept-default.sh mail/src/message.rs mail/src/cmds.rs
#
# A multi-line X-Spira-Default is folded by the writer; accepting it sends the full text.
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

W="$(mktemp -d)"
trap 'rm -rf "$W"' EXIT
mkdir "$W/bin"
printf '#!/bin/sh\ncat > "%s/sent"\n' "$W" > "$W/bin/mail"
chmod +x "$W/bin/mail"

folded=$'From: Builder <b@spira>\nSubject: line one\n line two\nX-Spira-Default: first paragraph\n \n second paragraph\n \n third paragraph\nMessage-ID: <m1@spira>\n\n## Question\nq\n'
printf '%s' "$folded" > "$W/msg"

if python3 -I -c '
import sys, email, email.policy
m = email.message_from_binary_file(open(sys.argv[1], "rb"), policy=email.policy.strict)
sys.exit(1 if m.defects else 0)' "$W/msg"; then
    ok "positive control: the folded fixture parses with no defects"
else
    bad "positive control: the folded fixture parses with no defects"
fi

raw=$'From: Builder <b@spira>\nSubject: s\nX-Spira-Default: first paragraph\nsecond paragraph\nMessage-ID: <m1@spira>\n\nbody\n'
printf '%s' "$raw" > "$W/raw"
if python3 -I -c '
import sys, email, email.policy
m = email.message_from_binary_file(open(sys.argv[1], "rb"), policy=email.policy.strict)
sys.exit(1 if m.defects else 0)' "$W/raw" 2>/dev/null; then
    bad "positive control: an unfolded multi-line header is detected as defective"
else
    ok "positive control: an unfolded multi-line header is detected as defective"
fi

PATH="$W/bin:$PATH" bash "$HERE/../aerc/accept-default.sh" < "$W/msg" >"$W/out" 2>&1
rc=$?
is "accept-default exits 0 on a folded default" "0" "$rc"
[ "$rc" = 0 ] || sed 's/^/    /' "$W/out"
want "reply carries the first paragraph" "first paragraph" "$(cat "$W/sent" 2>/dev/null)"
want "reply carries the third paragraph" "third paragraph" "$(cat "$W/sent" 2>/dev/null)"
want "reply subject carries both subject lines" "Subject: Re: line one line two" "$(cat "$W/sent" 2>/dev/null)"

tl_summary
