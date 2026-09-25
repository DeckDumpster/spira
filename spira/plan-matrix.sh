#!/usr/bin/env bash
# plan-matrix.sh — regenerate docs/test-plan/coverage.json and COVERAGE.md whole from the
# typed catalogues, every suite's # tier:/# covers: header, and best-effort tsd timings.
#
#   plan-matrix.sh          regenerate and write both files
#   plan-matrix.sh --check  regenerate into scratch, diff against the committed files;
#                           exit 1 naming the diff if they differ (never writes)
#
# NEVER HAND-EDIT docs/test-plan/coverage.json or COVERAGE.md — both are derived
# (law-regenerate-derived-summaries). --check is what the gate runs, so a stale copy fails.
#
# tier: T0
# covers: spira/plan-matrix.sh docs/test-plan/coverage.json docs/test-plan/COVERAGE.md
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
ROOT="$(git -C "$HERE" rev-parse --show-toplevel 2>/dev/null)"
[ -n "$ROOT" ] || ROOT="$(cd "$HERE/.." && pwd -P)"
. "$HERE/test-plan-bin.sh"
DOCS_DIR="$ROOT/docs/test-plan"
JSON_OUT="$DOCS_DIR/coverage.json"
MD_OUT="$DOCS_DIR/COVERAGE.md"

check=0
case "${1:-}" in
    --check) check=1 ;;
    "") ;;
    *) printf 'plan-matrix.sh: unknown argument %s\n' "$1" >&2; exit 2 ;;
esac

bin="$(resolve_test_plan_bin)" || exit 1

tmp="$(mktemp -d)"; trap 'rm -rf "$tmp"' EXIT
bash "$HERE/suite-coverage-json.sh" > "$tmp/suites.json"
bash "$HERE/tsd-timings-json.sh" > "$tmp/timings.json"

if ! "$bin" matrix --catalogue-dir "$DOCS_DIR" --suites "$tmp/suites.json" \
    --timings "$tmp/timings.json" > "$tmp/coverage.json"
then
    printf 'plan-matrix.sh: matrix generation failed\n' >&2
    exit 1
fi
if ! "$bin" render --matrix "$tmp/coverage.json" > "$tmp/COVERAGE.md"; then
    printf 'plan-matrix.sh: markdown render failed\n' >&2
    exit 1
fi

if [ "$check" = 1 ]; then
    bad=0
    if ! diff -u "$JSON_OUT" "$tmp/coverage.json" 2>&1; then
        printf 'plan-matrix.sh: %s is stale — regenerate with `spira/plan-matrix.sh`\n' \
            "${JSON_OUT#"$ROOT"/}" >&2
        bad=1
    fi
    if ! diff -u "$MD_OUT" "$tmp/COVERAGE.md" 2>&1; then
        printf 'plan-matrix.sh: %s is stale — regenerate with `spira/plan-matrix.sh`\n' \
            "${MD_OUT#"$ROOT"/}" >&2
        bad=1
    fi
    [ "$bad" = 0 ] && printf 'plan-matrix.sh: coverage.json and COVERAGE.md are current\n'
    exit "$bad"
fi

cp "$tmp/coverage.json" "$JSON_OUT"
cp "$tmp/COVERAGE.md" "$MD_OUT"
printf 'plan-matrix.sh: wrote %s and %s\n' "${JSON_OUT#"$ROOT"/}" "${MD_OUT#"$ROOT"/}"
