#!/usr/bin/env bash
#
# test-wire-token.sh — SENT is the wire token between the Hel (hel/, was hel.sh),
# the sentinel, and
# cockpit-collect metrics. Three programs parse each other's stdout; a rename that lands in the
# emitter but not both parsers yields ZERO rather than an error, so the failure mode is
# silence rather than a crash. This suite is the positive control: it fails the moment any of
# the three disagrees about the token name.
#
# WHY A STATIC CHECK, NOT AN INTEGRATION RUN
# -------------------------------------------
# The silent-zero failure mode is precisely what an integration run would miss: if hel.sh
# emits REAPED and sentinel.sh greps for SENT, the sentinel logs "sent 0 branches" and exits
# 0, which is the ordinary idle-pass output. Only a check that reads ALL THREE files and
# verifies they share one token can detect the drift before it reaches the log.
#
# WHAT COUNTS AS THE EMITTER LINE
# --------------------------------
# hel.sh emits `say "SENT …"` lines; the token is the first word of that argument.
# sentinel.sh greps `^SENT`; cockpit-collect metrics does `t.starts_with("SENT")`.
# Each grep below targets the functional form — the literal string as it would appear in the
# source — so a rename that updates every grep but not the say, or vice versa, is caught here.
#
# THE CONTRACT ROW pipes one canned SENT line — shaped exactly as send_branch() emits it —
# through both real parsers. cockpit-collect metrics's hel_metrics() is a pure function,
# called directly, no source-grep. sentinel.sh has no equivalent extracted function (its
# parsing is inline in a large conditional block); the grep/field-split idiom below is
# copied from its exact source line rather than hand-typed, so a source-grep still confirms
# neither side drifted from the copy.
#
# defect: sp-dcfm
# tier: T1
# tier: T0
# covers: hel/src/* sentinel/src/* cockpit-collect/src/metrics.rs UC-ops-detection-remediation-36
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

echo "test-wire-token.sh"

HEL="$HERE/../hel/src/sweep.rs"   # the Hel is Rust (was hel.sh, sp-arpjt)
METRICS="$HERE/../cockpit-collect/src/metrics.rs"

# ---------------------------------------------------------------------------------------
# EMITTER. The Hel emits `SENT …` lines — `self.landed(.., "SENT")` for every landed
# arm, and `format!("SENT {id} …")` for an orphaned worktree.
# ---------------------------------------------------------------------------------------
if grep -qF 'self.send_landed(c, id, br, "SENT")' "$HEL" && grep -qF 'format!("SENT {id}' "$HEL"; then
    ok "the Hel emits SENT"
else
    bad "the Hel emits SENT" "\"SENT\" verb not found in $HEL — emitter is out of sync"
fi

# NEGATIVE: the old token must never be a landed line's own verb.
if grep -qF 'format!("REAPED' "$HEL"; then
    bad "the Hel no longer emits REAPED as a sent line" "a REAPED emitter was found"
else
    ok "the Hel does not emit REAPED as a sent line"
fi

# ---------------------------------------------------------------------------------------
# PARSER — cockpit-metrics starts_with the emitter's token.
# ---------------------------------------------------------------------------------------
if grep -q 'starts_with("SENT")' "$METRICS"; then
    ok "cockpit-collect metrics starts_with(\"SENT\")"
else
    bad "cockpit-collect metrics starts_with(\"SENT\")" "starts_with(\"SENT\") not found — metrics parser is out of sync"
fi

# NEGATIVE: old parse prefix must be gone.
if grep -q 'starts_with("REAPED")' "$METRICS"; then
    bad "cockpit-collect metrics no longer starts_with(\"REAPED\")" "starts_with(\"REAPED\") still present"
else
    ok "cockpit-collect metrics does not starts_with(\"REAPED\")"
fi

# ---------------------------------------------------------------------------------------
# CONTRACT ROW — one canned SENT line, piped through the real parser.
# ---------------------------------------------------------------------------------------
FIXTURE_LINE='SENT sp-test1  myrepo mybranch  orphaned worktree (branch was already gone)'

COLLECT="$(command -v cockpit-collect)"
NOW_TS="$(date -u '+%Y-%m-%dT%H:%M:%SZ')"
TMPD="$(mktemp -d)"; trap 'rm -rf "$TMPD"' EXIT
printf '%s\n' "$NOW_TS spira: state: open=0 plan_ready=0 in_progress=0 dwarves=0" "$FIXTURE_LINE" > "$TMPD/sentinel.log"
: > "$TMPD/ledger.log"
out="$("$COLLECT" metrics "$TMPD/sentinel.log" "$TMPD/ledger.log" 24 2>/dev/null | sed -n 's/^SP_SENT=//p')"
is "cockpit-collect metrics counts the fixture SENT line" "1" "$out"

tl_summary
