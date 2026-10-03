#!/usr/bin/env bash
#
# test-stranded-runner.sh — the stranded-runner watchd daemon reports what the forge's
# stranded-runners verb finds, writes a health record, and the health probe fails when the
# loop stops. The detection itself (jobs+runners API fixtures) is unit-tested in forge/src/tests.rs.
#
# tier: T1
# covers: spira/stranded-runner.sh spira/watchers forge/src/cmds.rs
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

echo "test-stranded-runner.sh"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

cat > "$TMP/forge" <<'STUB'
#!/usr/bin/env bash
[ "$1" = "stranded-runners" ] || exit 2
cat "$STUB_OUT"
STUB
chmod +x "$TMP/forge"

run() {
    env -i PATH="$PATH" HOME="$TMP" SPIRA_RUN="$TMP/run" SPIRA_REPO="$TMP" \
        SPIRA_FORGE="$TMP/forge" STUB_OUT="$TMP/out" bash "$HERE/stranded-runner.sh" "$@" 2>&1
}

printf 'STRANDED run=9001 job="suites" label=spira-run-9001 queued=360s runner=eph-133742 vmid=133742\n' > "$TMP/out"
want "positive control: a stranded job is reported with run, job and vmid" \
    "vmid=133742" "$(run --show)"

: > "$TMP/out"
is "silent when the forge finds nothing" "" "$(run --show)"

run watch --interval 7 --ticks 1 >/dev/null
want "watch writes a health record" "ok " "$(cat "$TMP/run/watchd/stranded-runner.health")"
run health >/dev/null; is "health passes after a fresh poll" "0" "$?"

echo "ok 1 100 7" > "$TMP/run/watchd/stranded-runner.health"
run health >/dev/null; is "health fails once the last poll is stale" "1" "$?"

grep -q '^stranded-runner|daemon|' "$HERE/watchers" && ok=0 || ok=1
is "watchers manifest carries the stranded-runner row" "0" "$ok"

tl_summary
