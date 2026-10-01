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
#   2. When fayth_ready asks ready_count for the builder, qa-proposed appears in the
#      exclude argument (the end-to-end predicate check).
#
# WHAT THIS SUITE DOES NOT USE. No real database, no systemd, no network. `fayth_ready`
# (wave 4.25, sp-obhv6: a one-line shim onto `spira-claim fayth-ready`, no longer an
# in-process bash call to `ready_count`) is exercised end to end against a fake `$SPIRA_BD`
# that records its own argv — one layer lower than the old `ready_count` function stub, but
# the same technique test-sentinel-store-reads.sh already uses.
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
export SPIRA_RUN="$T/run"
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
echo "fayth_ready builder — qa-proposed appears in the exclude argument to ready_count"
# ==========================================================================================
# A fake bd that records its own argv: `fayth-ready` calls `ready_count`'s Rust equivalent
# with `--label <FAYTH_LABELS> --exclude-label <fayth_exclude ...>`, so qa-proposed (from
# builder.fayth's own FAYTH_EXCLUDE_LABELS) must appear there.
BD_ARGS_FILE="$T/observed-bd-args"
FAKE_BD="$T/fake-bd"
cat > "$FAKE_BD" <<EOF
#!/bin/sh
printf '%s\n' "\$*" >> "$BD_ARGS_FILE"
echo '[]'
EOF
chmod +x "$FAKE_BD"

SPIRA_BD="$FAKE_BD" SPIRA_DB="/fake/db" fayth_ready builder >/dev/null 2>&1 || true
observed_excl="$(cat "$BD_ARGS_FILE" 2>/dev/null)"
want "fayth_ready builder passes qa-proposed in the exclude arg to bd" "qa-proposed" "$observed_excl"

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
