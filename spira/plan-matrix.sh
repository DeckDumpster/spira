#!/usr/bin/env bash
# plan-matrix.sh — regenerate docs/test-plan/coverage.json and COVERAGE.md whole from the
# typed catalogues, every suite's # tier:/# covers: header, and best-effort tsd timings.
#
# Both outputs are untracked (.gitignore) and rewritten on every call — there is no committed
# copy to compare against or go stale, so two branches that each regenerate them independently
# can never conflict or disagree on merge (law-test-selection-and-plan-are-one-source). The
# catalogues and suite headers are the one source; these files are always a fresh view of it.
#
# THE GENERATION ITSELF (build the matrix, render it, write both files) is the test-plan
# crate's `write-matrix` (sp-pppt0): one process instead of two binary calls plus two `cp`s,
# so the two documents can never disagree with each other the way separate writes briefly
# could. This script's own job is gathering the two JSON inputs a shell script is already the
# one source for (the suite headers via git, the tsd timings via tsd-query.sh).
#
# tier: T0
# covers: spira/plan-matrix.sh docs/test-plan/coverage.json docs/test-plan/COVERAGE.md
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
ROOT="$(git -C "$HERE" rev-parse --show-toplevel 2>/dev/null)"
[ -n "$ROOT" ] || ROOT="$(cd "$HERE/.." && pwd -P)"
DOCS_DIR="$ROOT/docs/test-plan"
JSON_OUT="$DOCS_DIR/coverage.json"
MD_OUT="$DOCS_DIR/COVERAGE.md"

case "${1:-}" in
    "") ;;
    *) printf 'plan-matrix.sh: unknown argument %s\n' "$1" >&2; exit 2 ;;
esac

tmp="$(mktemp -d)"; trap 'rm -rf "$tmp"' EXIT
bash "$HERE/suite-coverage-json.sh" > "$tmp/suites.json"
bash "$HERE/tsd-timings-json.sh" > "$tmp/timings.json"

if ! test-plan write-matrix --catalogue-dir "$DOCS_DIR" --suites "$tmp/suites.json" \
    --timings "$tmp/timings.json" --out-json "$JSON_OUT" --out-md "$MD_OUT"
then
    printf 'plan-matrix.sh: matrix generation failed\n' >&2
    exit 1
fi
printf 'plan-matrix.sh: wrote %s and %s\n' "${JSON_OUT#"$ROOT"/}" "${MD_OUT#"$ROOT"/}"
