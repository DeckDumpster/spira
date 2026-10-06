#!/usr/bin/env bash
#
# test-builder-qa-proposed.sh — builder.fayth must exclude qa-proposed so that
#   a QA proposal cannot be claimed as approved work.
#
#   ./test-builder-qa-proposed.sh
#
# THE DEFECT. qa.md tells the QA aeon to file proposals labelled spira,plan,qa-proposed.
# plan IS builder.fayth's partition predicate, and qa-proposed was not in FAYTH_EXCLUDE_LABELS,
# so a proposal was indistinguishable from approved work the instant it was written. A builder
# claimed one; while working it filed more proposals; those summoned more builders. 247
# qa-proposed beads were filed in eight hours by ten builder aeons. (sp-2mhcs)
#
# THE FIX. Add qa-proposed to FAYTH_EXCLUDE_LABELS in builder.fayth.
#
# WHAT THIS SUITE CHECKS.
#   1. builder.fayth's FAYTH_EXCLUDE_LABELS contains qa-proposed (the direct field check).
#   2. fayth_ready builder, over a ready set holding one plain plan bead and one QA proposal,
#      counts the plan bead and never the proposal (the end-to-end predicate check).
#
# WHAT THIS SUITE DOES NOT USE. No real database, no systemd, no network. `fayth_ready` is a
# one-line shim onto `spira-claim fayth-ready` (wave 4.25, sp-obhv6), and its ready set is the
# lifecycle machine's READY rows (sp-v62vn: there is no off mode, so no `bd ready` call whose
# argv this suite could read): a spira-lc stand-in (testlib `lc_mirror_bd`) answers `list`
# from a fake `$SPIRA_BD`'s two fixture beads, and spira-claim's own label predicate does
# the excluding.
#
# tier: T1
# covers: spira/chamber/builder.fayth spira/lib.sh spira-claim/*
# hermetic-ok: no real database, no systemd; SPIRA_BD and SPIRA_SUMMON are stubs
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

lack() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT INT TERM
mkdir -p "$T/run"

export SPIRA_HOME="$HERE"
# SPIRA_CHAMBER no longer derives from SPIRA_HOME (the fixture declares its own path) —
# point it at the real chamber this suite reads fayths from.
tl_config SPIRA_RUN="$T/run" SPIRA_CHAMBER="$HERE/chamber"
export SPIRA_CONF="$T/no-such.conf"
# shellcheck disable=SC1090
. "$HERE/lib.sh"

echo "test-builder-qa-proposed.sh"

# ==========================================================================================
echo
echo "builder.fayth — FAYTH_EXCLUDE_LABELS contains qa-proposed"
# ==========================================================================================
# A POSITIVE CONTROL AGAINST THE REAL FILE. This assertion fails if qa-proposed is removed
# from FAYTH_EXCLUDE_LABELS in builder.fayth, which is the field the predicate reads.
builder_excl="$(fayth_get builder FAYTH_EXCLUDE_LABELS)"
want "builder FAYTH_EXCLUDE_LABELS contains qa-proposed" "qa-proposed" "$builder_excl"

# Positive control: the include labels are still correct.
builder_labels="$(fayth_get builder FAYTH_LABELS)"
want "builder FAYTH_LABELS contains plan" "plan" "$builder_labels"

# ==========================================================================================
echo
echo "fayth_ready builder — a qa-proposed bead is not in the builder's ready set"
# ==========================================================================================
# Two READY beads carrying the scope and plan labels: one plain, one also labelled
# qa-proposed. The machine says both are READY; builder.fayth's own FAYTH_EXCLUDE_LABELS is
# the only thing that may tell them apart. SPIRA_SCOPE_LABEL is pinned to the fixture's scope
# label below, so builder.fayth's FAYTH_LABELS (scope + plan) is a predicate both satisfy.
lbl_json='"spira","plan",'
FIXTURE="$T/beads.json"
cat > "$FIXTURE" <<EOF
[{"id":"sp-qaplain","title":"approved plan work","status":"open","issue_type":"task","priority":1,"labels":[${lbl_json}"repo:spira"]},
 {"id":"sp-qaprop","title":"a QA proposal","status":"open","issue_type":"task","priority":1,"labels":[${lbl_json}"repo:spira","qa-proposed"]}]
EOF
FAKE_BD="$T/fake-bd"
cat > "$FAKE_BD" <<EOF
#!/bin/sh
case " \$* " in *" list "*) cat "$FIXTURE" ;; *) echo '[]' ;; esac
EOF
chmod +x "$FAKE_BD"
lc_mirror_bd "$T/lc"
tl_config SPIRA_DB="/fake/db" SPIRA_SCOPE_LABEL=spira SPIRA_BD="$FAKE_BD"

ready_builder() {   # ready_builder [--json]
    PATH="$T/lc:$PATH" \
        _spira_claim fayth-ready builder "$@" 2>/dev/null
}
# POSITIVE CONTROL: the fixture reaches spira-claim at all — the plain bead counts. A machine
# that answered nothing would read 0 and make the exclusion below vacuous.
is "positive control: fayth_ready builder counts the plain plan bead" "1" \
    "$(PATH="$T/lc:$PATH" fayth_ready builder 2>/dev/null)"
builder_set="$(ready_builder --json)"
want   "fayth_ready builder's set holds the plain plan bead" "sp-qaplain" "$builder_set"
nowant "fayth_ready builder's set excludes the qa-proposed bead" "sp-qaprop" "$builder_set"

# ==========================================================================================
echo
echo "positive control — qa-proposed is NOT in ops.fayth exclusions (it must be deliberate)"
# ==========================================================================================
# Proves the label is not in the shared base — if it were, the builder fix would be trivially
# satisfied by something unrelated, and removing it from builder.fayth would not be detected.
ops_excl="$(fayth_get ops FAYTH_EXCLUDE_LABELS)"
lack "ops FAYTH_EXCLUDE_LABELS does not contain qa-proposed (distinct from builder)" \
     "qa-proposed" "$ops_excl"

echo
tl_summary
