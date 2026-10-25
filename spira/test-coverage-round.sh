#!/usr/bin/env bash
#
# test-coverage-round.sh — the round's coverage artifacts: JSON and text beside the results, the
# delta against the last green round, no kept report from a red round, and a missing line report
# reading "lines: unavailable" instead of failing.
#
# host-reason: reads scratch directories only; no database, no systemd
#
# tier: T1
# covers: spira/coverage-round.sh test-plan/
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

echo "test-coverage-round.sh"
for _t in test-plan suite-select; do
    command -v "$_t" >/dev/null 2>&1 || bail "$_t is not on PATH"
done

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
TREE="$TMP/tree"; STATE="$TMP/state"
mkdir -p "$TREE/spira" "$TREE/docs/test-plan" "$STATE"
cp "$HERE/suite-coverage-json.sh" "$TREE/spira/"
git init -q -b main "$TREE"
cat > "$TREE/docs/test-plan/dispatch.toml" <<'TOML'
api_version = "test-plan/v1"
area = "dispatch"

[[use_case]]
id = "UC-dispatch-01"
tier = "T1"
statement = "a claim writes a lease"

[[use_case]]
id = "UC-dispatch-02"
tier = "T1"
statement = "a release clears it"
TOML
suite() {  # suite <uc-line>
    printf '#!/usr/bin/env bash\n# tier: T1\n# covers: %s\nset -uo pipefail\n' "$1" > "$TREE/spira/test-a.sh"
    git -C "$TREE" add -A && git -C "$TREE" commit -q -m c --allow-empty
}
lines() {  # lines <dir> <covered>
    mkdir -p "$1"
    printf '{"data":[{"totals":{"lines":{"count":10,"covered":%s}},"files":[{"filename":"/vm/work/src/x.rs","summary":{"lines":{"count":10,"covered":%s}}}]}]}' "$2" "$2" > "$1/coverage-lines.json"
    echo /vm/work > "$1/coverage-root"
}
run() { env -i PATH="$PATH" HOME="$TMP" SPIRA_METRICS_ENDPOINT= bash "$HERE/coverage-round.sh" "$TREE" "$@" 2>&1; }
git -C "$TREE" config user.email t@t; git -C "$TREE" config user.name t

suite "UC-dispatch-01"
lines "$TMP/r1" 5
out="$(run "$TMP/r1" "$STATE" r1 1)"
want "first round reports lines" "coverage: lines: 50.00% (5/10)" "$out"
want "first round reports use cases" "coverage: use cases: 1/2 covered" "$out"
nowant "first round has no delta" "delta:" "$out"
want "uncovered ids are listed" "uncovered: UC-dispatch-02" "$(cat "$TMP/r1/coverage.txt")"
want "file names lose the VM root" '"src/x.rs"' "$(cat "$TMP/r1/coverage.json")"
[ -s "$STATE/coverage-latest.json" ] && ok "a green round keeps its report" || bad "a green round keeps its report" "missing"

suite "UC-dispatch-01 UC-dispatch-02"
lines "$TMP/r2" 7
out="$(run "$TMP/r2" "$STATE" r2 0)"
want "second round carries the delta line" "coverage: delta: lines 70.00% (+20.00 pts); use cases 2/2 (+1 gained, -0 lost)" "$out"
want "delta text names the moved file" "file src/x.rs" "$(cat "$TMP/r2/coverage.txt")"
want "a red round does not replace the kept report" '"round": "r1"' "$(cat "$STATE/coverage-latest.json")"

mkdir -p "$TMP/r3"
out="$(run "$TMP/r3" "$STATE" r3 1)"
want "a round with no line data says unavailable" "coverage: lines: unavailable" "$out"
want "and still reports use cases" "coverage: use cases: 2/2" "$out"
want "and its delta reports lines unavailable" "lines unavailable" "$out"

tl_summary
