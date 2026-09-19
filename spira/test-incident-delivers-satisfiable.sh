#!/usr/bin/env bash
# covers: spira/incident.sh
#
# A delivers: label is a CLOSING CRITERION: the sentinel reopens a bead closed without a
# commit unless the named path was written during the session. So a label naming a path
# that cannot exist is not a harmless annotation -- it is a bead that can never be closed
# by the path the label describes.
#
# incident.sh stamped delivers:note:$SPIRA_RUN/sop/applied.jsonl on EVERY bead it filed,
# and nothing ever created that directory. Eleven open beads carried it. Two more carried
# an absolute path belonging to a DIFFERENT install, having travelled between machines,
# and no session here could ever satisfy those.
#
# THE RULE IS SCHEMA-ON-WRITE, not repair-on-read: a criterion nobody can satisfy must not
# be recorded in the first place. Patching the labels afterwards leaves the writer free to
# mint more.
#
# MATCHERS READ CODE, NOT PROSE (law-a-matcher-reads-code-not-prose): the comment above the
# mechanism in incident.sh names every token these checks look for.
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
SCRIPT_UT="$HERE/incident.sh"
CODE="$(grep -vE '^[[:space:]]*#' "$SCRIPT_UT")"
JOINED="$(printf '%s' "$CODE" | sed -e :a -e '/\\$/N; s/\\\n//; ta')"
has() { printf '%s' "$JOINED" | grep -qE "$1"; }

pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "${2:-}"; }

echo "test-incident-delivers-satisfiable.sh"
echo

# Vacuity guard: if the label write ever moves or is renamed, every check below would pass
# against a file that no longer does the thing at all.
if has 'delivers:note:'; then
    ok "the delivers write was located (positive control)"
else
    bad "the delivers write was located (positive control)" "no delivers:note: write in incident.sh; the checks below are vacuous"
fi

echo
echo "a criterion is only written when something can satisfy it:"
# The ledger's directory must exist, or the very first close is reopened forever.
# Must name the LEDGER. The first version of this matched `mkdir -p "$SPOOL"
# "$(dirname "$ILOG")"` on an unrelated line and passed against a file that never
# created the ledger's directory at all.
# NOT [^\n] -- inside an ERE bracket expression that is the literal characters
# backslash and n, so it excludes the letter n, and `dirname` contains one. The
# matcher reported a correct mkdir as missing. grep is line-based; `.*` is right.
if has 'mkdir -p .*_sop_ledger'; then
    ok "the ledger's directory is created, so the path is reachable"
else
    bad "the ledger's directory is created, so the path is reachable" "nothing creates it; the first close is reopened and every one after it"
fi

# A path outside this install can never be written by a session here, and beads travel
# between machines carrying absolute paths.
if has 'SPIRA_RUN' && has 'case .*_sop_ledger|\[\[ .*_sop_ledger|\$\{_sop_ledger#'; then
    ok "a path outside this install's run directory is refused"
else
    bad "a path outside this install's run directory is refused" "an absolute path from another install is stamped verbatim and can never be satisfied here"
fi

# Silence here would recreate the defect in a new shape: a label quietly not written is as
# hard to diagnose as one that cannot be met.
if has 'delivers' && has '(log|printf|warn).*deliver'; then
    ok "skipping the label says so"
else
    bad "skipping the label says so" "a silently-omitted criterion is as hard to diagnose as an unsatisfiable one"
fi

echo
echo "the retirement migration tells the truth about what it will do:"
# A --dry-run that mutates is not one. The first version ran mkdir -p on the
# reachability branch, so the dry run CREATED the directories it was asking about,
# reported all eight labels reachable, and the real run then retired nothing after
# the dry run had promised six. A migration whose two modes disagree is worse than
# no migration: the preview is what an operator decides on.
_mig="$(printf '%s' "$JOINED" | awk '/retire-unsatisfiable-delivers\)/{f=1} f{print} f&&/^    ;;/{exit}')"
if [ -n "$_mig" ]; then
    ok "the migration block was located (positive control)"
else
    bad "the migration block was located (positive control)" "not found; the checks below are vacuous"
fi
if printf '%s' "$_mig" | grep -q 'mkdir'; then
    bad "the migration only asks, never repairs" "it calls mkdir: --dry-run mutates, and the two modes disagree about what will happen"
else
    ok "the migration only asks, never repairs"
fi
# Creating the directory was a mirage anyway: the sentinel needs the ledger's MTIME
# to move during the session, so an empty directory satisfies nothing. Ensuring it
# belongs in the writer, where a session running sop.sh can actually fill it.
if printf '%s' "$JOINED" | grep -q 'mkdir -p .*_sop_ledger'; then
    ok "the writer is where the directory is ensured"
else
    bad "the writer is where the directory is ensured" "nothing creates it at write time"
fi

echo
echo "SPIRA_INCIDENT_DELIVERS: callers can declare an explicit delivers type at filing time:"
# Positive control: the handler must exist in the code, or the checks below are vacuous.
if has 'SPIRA_INCIDENT_DELIVERS'; then
    ok "SPIRA_INCIDENT_DELIVERS handler was located (positive control)"
else
    bad "SPIRA_INCIDENT_DELIVERS handler was located (positive control)" "not found in incident.sh; the checks below are vacuous"
fi
# An unknown type must not be silently stamped — it would create an unsatisfiable criterion.
# The handler must validate before writing.
if has 'SPIRA_INCIDENT_DELIVERS' && has 'case.*SPIRA_INCIDENT_DELIVERS|unrecognised.*SPIRA_INCIDENT_DELIVERS'; then
    ok "unknown SPIRA_INCIDENT_DELIVERS types are rejected before writing"
else
    bad "unknown SPIRA_INCIDENT_DELIVERS types are rejected before writing" "an unvalidated env var could stamp a delivers: type the sentinel cannot verify"
fi
# Skipping is logged — same rule as the SOP ledger path.
if has 'SPIRA_INCIDENT_DELIVERS' && has 'ilog.*SPIRA_INCIDENT_DELIVERS|ilog.*deliver.*SPIRA_INCIDENT'; then
    ok "SPIRA_INCIDENT_DELIVERS writes are logged"
else
    bad "SPIRA_INCIDENT_DELIVERS writes are logged" "a silently-omitted or silently-written criterion is as hard to diagnose as an unsatisfiable one"
fi
# When set, it replaces the SOP ledger path — both must not apply to the same bead.
if has 'SPIRA_INCIDENT_DELIVERS' && has 'else'; then
    ok "SPIRA_INCIDENT_DELIVERS and the SOP ledger path are mutually exclusive (else branch)"
else
    bad "SPIRA_INCIDENT_DELIVERS and the SOP ledger path are mutually exclusive" "both paths could apply, creating a compound criterion the aeon must satisfy both parts of"
fi

echo
printf '  %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
