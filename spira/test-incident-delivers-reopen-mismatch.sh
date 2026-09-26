#!/usr/bin/env bash
# covers: spira/incident.sh spira/lib.sh
#
# REGRESSION: incident.sh stamped delivers:note:applied.jsonl on every incident by default,
# so a diagnosis-only incident was reopened when it closed — the criterion could not be met
# by correct work. The check was also time-based, not identity-based, so a concurrent
# unrelated SOP application satisfied the criterion by coincidence.
#
# Seen to fail against the unfixed tree (commit sp-qqf3q):
#   FAIL  default stamps delivers:action, not the SOP ledger: not found in incident.sh else branch
#   FAIL  lib.sh uses identity check for applied.jsonl, not mtime: not found in delivers_verdict's note case
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
INC="$HERE/incident.sh"
LIB="$HERE/lib.sh"
INC_CODE="$(grep -vE '^[[:space:]]*#' "$INC")"
INC_JOINED="$(printf '%s' "$INC_CODE" | sed -e :a -e '/\\$/N; s/\\\n//; ta')"
LIB_CODE="$(grep -vE '^[[:space:]]*#' "$LIB")"
LIB_JOINED="$(printf '%s' "$LIB_CODE" | sed -e :a -e '/\\$/N; s/\\\n//; ta')"
has_inc() { grep -qE "$1" <<< "$INC_JOINED"; }
has_lib() { grep -qE "$1" <<< "$LIB_JOINED"; }

pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "${2:-}"; }

echo "test-incident-delivers-reopen-mismatch.sh"
echo

echo "incident.sh default stamps delivers:action, not the SOP ledger:"
# The default path (no SPIRA_INCIDENT_DELIVERS set) must write delivers:action.
# Previously it wrote delivers:note:applied.jsonl, which caused correct diagnostic
# work to be reopened for failing to produce an SOP ledger record.
# The ilog for the default must name both "delivers:action" and "default".
if has_inc 'ilog.*delivers:action.*default|ilog.*default.*delivers:action'; then
    ok "default stamps delivers:action, not the SOP ledger"
else
    bad "default stamps delivers:action, not the SOP ledger" \
        "no ilog for delivers:action default path in incident.sh"
fi

# The SOP ledger path must only be reachable via SPIRA_INCIDENT_DELIVERS=note.
# If it exists outside that guard, an unguarded incident gets an unsatisfiable criterion.
if has_inc 'SPIRA_INCIDENT_DELIVERS.*note|note\)'; then
    ok "delivers:note SOP ledger path is behind SPIRA_INCIDENT_DELIVERS=note guard"
else
    bad "delivers:note SOP ledger path is behind SPIRA_INCIDENT_DELIVERS=note guard" \
        "SOP ledger criterion must be opt-in, not default"
fi

echo
echo "lib.sh uses identity check for applied.jsonl, not mtime:"
# The shared global SOP ledger mtime proves only that someone applied some SOP during
# this session — a concurrent unrelated aeon satisfies the criterion for free.
# The check must require a record naming this specific bead.
# Positive control: a dedicated code path for applied.jsonl exists (absent means unfixed).
if has_lib 'applied\.jsonl'; then
    ok "lib.sh has a dedicated code path for applied.jsonl (positive control)"
else
    bad "lib.sh has a dedicated code path for applied.jsonl (positive control)" \
        "no applied.jsonl branch in lib.sh — identity check is absent"
fi
# That branch must reference the bead id parameter to prove it checks the record's bead field.
if has_lib 'applied\.jsonl' && has_lib '"bead".*_id'; then
    ok "lib.sh applied.jsonl branch references the bead id for identity check"
else
    bad "lib.sh applied.jsonl branch references the bead id for identity check" \
        "bead id not referenced near the applied.jsonl check"
fi

echo
printf '  %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
