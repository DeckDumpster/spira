#!/usr/bin/env bash
#
# test-wire-token.sh — SENT is the wire token between sending.sh, sentinel.sh, and
# cockpit-metrics.py. Three programs parse each other's stdout; a rename that lands in the
# emitter but not both parsers yields ZERO rather than an error, so the failure mode is
# silence rather than a crash. This suite is the positive control: it fails the moment any of
# the three disagrees about the token name.
#
# WHY A STATIC CHECK, NOT AN INTEGRATION RUN
# -------------------------------------------
# The silent-zero failure mode is precisely what an integration run would miss: if sending.sh
# emits REAPED and sentinel.sh greps for SENT, the sentinel logs "sent 0 branches" and exits
# 0, which is the ordinary idle-pass output. Only a check that reads ALL THREE files and
# verifies they share one token can detect the drift before it reaches the log.
#
# WHAT COUNTS AS THE EMITTER LINE
# --------------------------------
# sending.sh emits `say "SENT …"` lines; the token is the first word of that argument.
# sentinel.sh greps `^SENT`; cockpit-metrics.py does `t.startswith("SENT")`.
# Each grep below targets the functional form — the literal string as it would appear in the
# source — so a rename that updates every grep but not the say, or vice versa, is caught here.
#
# defect: sp-dcfm
# tier: T1
# covers: spira/sending.sh spira/sentinel.sh spira/cockpit-metrics.py

# covers: spira/sending.sh spira/sentinel.sh spira/cockpit-metrics.py
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

echo "test-wire-token.sh"

SENDING="$HERE/sending.sh"
SENTINEL="$HERE/sentinel.sh"
METRICS="$HERE/cockpit-metrics.py"

# ---------------------------------------------------------------------------------------
# EMITTER. sending.sh must emit `say "SENT …"` — the wire token on the stdout channel.
# ---------------------------------------------------------------------------------------
if grep -q 'say "SENT ' "$SENDING"; then
    ok "sending.sh emits SENT"
else
    bad "sending.sh emits SENT" "say \"SENT \" not found — emitter is out of sync"
fi

# NEGATIVE: the old token must be gone from the emitter's say calls.
if grep -q 'say "REAPED' "$SENDING"; then
    bad "sending.sh no longer emits REAPED" "say \"REAPED found — old token still present"
else
    ok "sending.sh does not emit REAPED"
fi

# ---------------------------------------------------------------------------------------
# PARSER 1 — sentinel.sh greps the emitter's stdout and must look for ^SENT.
# ---------------------------------------------------------------------------------------
if grep -q "'^SENT'" "$SENTINEL"; then
    ok "sentinel.sh greps ^SENT"
else
    bad "sentinel.sh greps ^SENT" "'^SENT' not found — sentinel is out of sync with emitter"
fi

# NEGATIVE: old grep pattern must be gone.
if grep -q "'^REAPED'" "$SENTINEL"; then
    bad "sentinel.sh no longer greps ^REAPED" "'^REAPED' still present — old pattern survives"
else
    ok "sentinel.sh does not grep ^REAPED"
fi

# ---------------------------------------------------------------------------------------
# PARSER 2 — cockpit-metrics.py startswith the emitter's token.
# ---------------------------------------------------------------------------------------
if grep -q 'startswith("SENT")' "$METRICS"; then
    ok "cockpit-metrics.py startswith(\"SENT\")"
else
    bad "cockpit-metrics.py startswith(\"SENT\")" "startswith(\"SENT\") not found — metrics parser is out of sync"
fi

# NEGATIVE: old parse prefix must be gone.
if grep -q 'startswith("REAPED")' "$METRICS"; then
    bad "cockpit-metrics.py no longer startswith(\"REAPED\")" "startswith(\"REAPED\") still present"
else
    ok "cockpit-metrics.py does not startswith(\"REAPED\")"
fi
tl_summary
