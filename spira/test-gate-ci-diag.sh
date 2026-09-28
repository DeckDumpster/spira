#!/usr/bin/env bash
# test-gate-ci-diag.sh — gate.yml wires its Diagnostics step to gate-diag.sh by name.
#
# Sections 1-8 (gate-diag.sh's own FAIL-line, annotation, summary and retry-classification
# behavior) retired sp-ubw2o: that logic is now the Rust `gate-diag` crate
# (gate-diag/DESIGN.md), covered by `cargo test -p gate-diag` and the bead's parity run
# against the bash this replaced. This suite keeps only the one thing that was never about
# gate-diag.sh's own logic — that the workflow still calls it by name.
#
# tier: T0
# covers: .github/workflows/gate.yml UC-test-infrastructure-36
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
has() { case "$3" in *"$2"*) ok "$1" ;; *) bad "$1" "wanted [$2]"; esac; }

echo "test-gate-ci-diag.sh"
echo "gate.yml has artifact upload and diagnostics steps:"
GATE_YML="$HERE/../.github/workflows/gate.yml"
if [ ! -r "$GATE_YML" ]; then
    bad "gate.yml readable" "not found at .github/workflows/gate.yml"
else
    G="$(cat "$GATE_YML")"
    has "upload-artifact step present"     "upload-artifact"            "$G"
    has "artifact upload runs on always()" "always()"                   "$G"
    has "gate-diag.sh called in workflow"  "gate-diag.sh"               "$G"
    has "batch results directory uploaded" "batch"                      "$G"
fi

tl_summary
