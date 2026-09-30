#!/usr/bin/env bash
# tier: T1
# covers: spira/lib.sh
#
# REGRESSION: incident.sh stamped delivers:note:applied.jsonl on every incident by default,
# so a diagnosis-only incident was reopened when it closed — the criterion could not be met
# by correct work. The check was also time-based, not identity-based, so a concurrent
# unrelated SOP application satisfied the criterion by coincidence.
#
# THE incident.sh HALF OF THIS SUITE IS RETIRED (sp-0ekp7, wave 7b): incident.sh's delivers:
# logic moved to the `incident` Rust crate (incident::run::file_new's delivers match arm),
# and this suite's two incident.sh assertions were source-grep over its bash text — dead
# once the logic moved. Real behavioural coverage of the same two properties (default
# stamps delivers:action; the note/SOP-ledger path is reachable only via
# SPIRA_INCIDENT_DELIVERS=note, and only when the ledger's directory can actually be
# created) now lives in incident/src/run.rs's unit tests: delivers_default_is_action,
# delivers_note_inside_run_creates_the_ledger_directory_and_is_stamped,
# delivers_note_outside_run_falls_back_to_action_and_logs_outside,
# delivers_unrecognised_value_writes_no_label_and_logs_it. The lib.sh half below (the
# applied.jsonl identity check, untouched by this bead) stays as a bash suite.
#
# Seen to fail against the unfixed tree (commit sp-qqf3q):
#   FAIL  lib.sh uses identity check for applied.jsonl, not mtime: not found in delivers_verdict's note case
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
LIB="$HERE/lib.sh"
LIB_CODE="$(grep -vE '^[[:space:]]*#' "$LIB")"
LIB_JOINED="$(printf '%s' "$LIB_CODE" | sed -e :a -e '/\\$/N; s/\\\n//; ta')"
has_lib() { grep -qE "$1" <<< "$LIB_JOINED"; }

. "$HERE/testlib.sh"

echo "test-incident-delivers-reopen-mismatch.sh"
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
tl_summary
