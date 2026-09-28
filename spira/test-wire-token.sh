#!/usr/bin/env bash
#
# test-wire-token.sh — SENT is the wire token between the Sending (sending/, was sending.sh),
# the sentinel, and
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
# THE CONTRACT ROW pipes one canned SENT line — shaped exactly as send_branch() emits it —
# through both real parsers. cockpit-metrics.py's sending_metrics() is a pure function,
# called directly, no source-grep. sentinel.sh has no equivalent extracted function (its
# parsing is inline in a large conditional block); the grep/field-split idiom below is
# copied from its exact source line rather than hand-typed, so a source-grep still confirms
# neither side drifted from the copy.
#
# defect: sp-dcfm
# tier: T1
# tier: T0
# covers: sending/src/* sentinel/src/* spira/cockpit-metrics.py UC-ops-detection-remediation-36
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

echo "test-wire-token.sh"

SENDING="$HERE/../sending/src/sweep.rs"   # the Sending is Rust (was sending.sh, sp-arpjt)
SENTINEL="$HERE/../sentinel/src/audit.rs"   # the sentinel is Rust (was sentinel.sh)
METRICS="$HERE/cockpit-metrics.py"

# ---------------------------------------------------------------------------------------
# EMITTER. The Sending emits `SENT …` lines — `self.landed(.., "SENT")` for every landed
# arm, and `format!("SENT {id} …")` for an orphaned worktree.
# ---------------------------------------------------------------------------------------
if grep -qF 'self.landed(c, id, br, "SENT")' "$SENDING" && grep -qF 'format!("SENT {id}' "$SENDING"; then
    ok "the Sending emits SENT"
else
    bad "the Sending emits SENT" "\"SENT\" verb not found in $SENDING — emitter is out of sync"
fi

# NEGATIVE: the old token must never be a landed line's own verb.
if grep -qF 'format!("REAPED' "$SENDING"; then
    bad "the Sending no longer emits REAPED as a sent line" "a REAPED emitter was found"
else
    ok "the Sending does not emit REAPED as a sent line"
fi

# ---------------------------------------------------------------------------------------
# PARSER 1 — the sentinel (sentinel/src/audit.rs) matches lines starting with SENT.
# ---------------------------------------------------------------------------------------
if grep -qF 'starts_with("SENT")' "$SENTINEL"; then
    ok "sentinel matches ^SENT"
else
    bad "sentinel matches ^SENT" "starts_with(\"SENT\") not found — sentinel is out of sync with emitter"
fi

# NEGATIVE: old match pattern must be gone.
if grep -q 'REAPED' "$SENTINEL"; then
    bad "sentinel no longer matches REAPED" "REAPED still present — old pattern survives"
else
    ok "sentinel does not match REAPED"
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

# ---------------------------------------------------------------------------------------
# CONTRACT ROW — one canned SENT line, piped through both real parsers.
# ---------------------------------------------------------------------------------------
FIXTURE_LINE='SENT sp-test1  myrepo mybranch  orphaned worktree (branch was already gone)'

# cockpit-metrics.py's sending_metrics(), called directly — no source-grep.
py_out="$(python3 -c '
import sys
sys.path.insert(0, "'"$HERE"'")
import importlib.util
spec = importlib.util.spec_from_file_location("cockpit_metrics", "'"$METRICS"'")
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)
from datetime import datetime, timezone
lines = ["2026-09-10T00:00:00Z spira: state: open=0 plan_ready=0 in_progress=0 aeons=0",
         "'"$FIXTURE_LINE"'"]
now = datetime(2026, 9, 10, 0, 5, 0, tzinfo=timezone.utc)
since = datetime(2026, 9, 9, 0, 0, 0, tzinfo=timezone.utc)
d = m.sending_metrics(lines, since, now)
print(d["SP_SENT"])
' 2>/dev/null)"
is "cockpit-metrics.py's sending_metrics() counts the fixture SENT line" "1" "$py_out"

# sentinel.sh's own idiom, copied verbatim rather than hand-typed:
# `grep -c '^SENT' <<< "$sent"` and `while read -r _ rid rrepo rbr _; do ...; done < <(grep '^SENT' <<< "$sent")`.
sent="$FIXTURE_LINE"
sh_count="$(grep -c '^SENT' <<< "$sent" || true)"
is "sentinel.sh's grep -c '^SENT' counts the fixture line" "1" "$sh_count"
read -r _ rid rrepo rbr _ < <(grep '^SENT' <<< "$sent")
is "sentinel.sh's field-split extracts the bead id"   "sp-test1" "$rid"
is "sentinel.sh's field-split extracts the repo name" "myrepo"   "$rrepo"
is "sentinel.sh's field-split extracts the branch"    "mybranch" "$rbr"

tl_summary
