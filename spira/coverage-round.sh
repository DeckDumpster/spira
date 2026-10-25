#!/usr/bin/env bash
# coverage-round.sh <tree> <results-dir> <state-dir> <round-id> <passed:0|1> — the round's
# coverage artifacts: coverage.json and coverage.txt in <results-dir>, the delta against the
# last landed round's report (kept in <state-dir>/coverage-latest.json), and the metrics push.
# Reports only: it exits 0 whatever it finds, and replaces the kept report only for a green round.
#
# tier: T1
# covers: spira/coverage-round.sh test-plan/
set -uo pipefail
tree="${1:?tree}" results="${2:?results-dir}" state="${3:?state-dir}" round="${4:-}" passed="${5:-0}"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
latest="$state/coverage-latest.json"

tmp="$(mktemp -d)"; trap 'rm -rf "$tmp"' EXIT
bash "$HERE/suite-coverage-json.sh" > "$tmp/suites.json" 2>"$tmp/suites.err" \
    || { echo "coverage-round: suite headers unreadable: $(head -3 "$tmp/suites.err")" >&2; exit 0; }

args=(--catalogue-dir "$tree/docs/test-plan" --suites "$tmp/suites.json" --round "$round"
      --out-json "$results/coverage.json" --out-txt "$results/coverage.txt")
if [ -s "$results/coverage-lines.json" ]; then
    args+=(--llvm-cov "$results/coverage-lines.json" --root "$(head -1 "$results/coverage-root" 2>/dev/null)")
fi
[ -s "$latest" ] && args+=(--prev "$latest")

if ! test-plan coverage-report "${args[@]}" > "$tmp/report.out"; then
    echo "coverage-round: test-plan coverage-report failed" >&2
    exit 0
fi
sed 's/^/coverage: /' "$tmp/report.out" | grep -E '^coverage: (lines|use cases|delta):' >&2
if [ "$passed" = 1 ]; then
    mkdir -p "$state" && cp "$results/coverage.json" "$latest.tmp" && mv -f "$latest.tmp" "$latest"
fi
exit 0
