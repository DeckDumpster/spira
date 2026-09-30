#!/usr/bin/env bash
#
# test-incident-cause.sh — the fence that refuses a SPIRA_INCIDENT_REF site with no
# SPIRA_INCIDENT_CAUSE (UC-ops-detection-remediation-09).
#
#   ./test-incident-cause.sh
#
# incident-cause-lint.sh is a gate fence, not a suite of its own: like literal-lint.sh
# and testdb-mode-lint.sh, it costs nothing to run on every push and belongs in the T0
# lint stage rather than the certification suite list.
#
# THE POSITIVE CONTROL IS FIRST (law-absence-needs-a-positive-control). A checker that
# reports the real tree clean is indistinguishable from a mis-scoped glob, an off-by-one
# window, or a grep that matches nothing — a planted offender in a scratch directory must
# be named before the shipped tree's silence means anything.
#
# tier: T1
# covers: spira/incident-cause-lint.sh spira/repo-map.example
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

echo "test-incident-cause.sh"

LINT="$HERE/incident-cause-lint.sh"

# --- positive control: the checker must find a planted offender -----------------
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
OFFENDER="$TMP/offender.sh"
# A minimal env block with SPIRA_INCIDENT_REF but no SPIRA_INCIDENT_CAUSE — exactly
# the shape every real site had before this fix.
cat > "$OFFENDER" <<'SCRIPT'
SPIRA_DB="$SPIRA_DB" \
SPIRA_INCIDENT_TYPE=task \
SPIRA_INCIDENT_ACTOR=test \
SPIRA_INCIDENT_REPO=spira \
SPIRA_INCIDENT_REF=incident:planted-offender \
bash incident.sh file "planted test" -
SCRIPT

out="$(bash "$LINT" --dir "$TMP" 2>&1)"; rc=$?
is "SEEN RED: positive control — planted offender is refused" "1" "$rc"
want "and it names the file"                                  "offender.sh" "$out"

# Withdraw the plant. An empty directory is not clean, it is unchecked (sp-ufbkh): refuse.
rm -f "$OFFENDER"
out="$(bash "$LINT" --dir "$TMP" 2>&1)"; rc=$?
is "an empty scratch directory is a refusal, never clean" "3" "$rc"
# (below line 10: the fence's window is sed's <line-10>,<line+3>)
{ printf '#\n%.0s' $(seq 11); printf 'SPIRA_INCIDENT_CAUSE=y\nSPIRA_INCIDENT_REF=x\n'; } > "$TMP/declared.sh"
out="$(bash "$LINT" --dir "$TMP" 2>&1)"; rc=$?
is   "GREEN AFTER: a declared site is clean" "0" "$rc"
want "and the clean scan prints its positive control" "fence: incident-cause-lint checked 1 files" "$out"

# --- comments are prose, not code (law-a-matcher-reads-code-not-prose) -----------
# The shape that turned Concierge round 49 red: a COMMENT quoting the assignment,
# with no real SPIRA_INCIDENT_CAUSE anywhere near it.
COMMENTED="$TMP/commented.sh"
cat > "$COMMENTED" <<'SCRIPT'
# fixed by rewording the comment that used to quote
# SPIRA_INCIDENT_REF="closed-not-landed:$id" here
do_thing() { :; }
SCRIPT

out="$(bash "$LINT" --dir "$TMP" 2>&1)"; rc=$?
is   "GREEN: a comment quoting the assignment is not a site" "0" "$rc"
nowant "and it is not named as undeclared" "commented.sh" "$out"
rm -f "$COMMENTED"

# --- real property: the shipped tree has no undeclared sites ---------------------
out="$(bash "$LINT" 2>&1)"; rc=$?
is   "the shipped spira/ tree is clean" "0" "$rc"
[ "$rc" = 0 ] || printf '%s\n' "$out" >&2

# --- gate integration: a fence nothing invokes is a file --------------------------
want "the gate names this fence"     "spira/incident-cause-lint.sh" "$(cat "$HERE/repo-map.example")"
is   "and the fence script is readable" "0" "$([ -r "$LINT" ]; echo $?)"

tl_summary
