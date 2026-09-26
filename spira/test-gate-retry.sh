#!/usr/bin/env bash
# test-gate-retry.sh — a red suite is re-run once serially; red twice fails, red then green passes.
# tier: T1
# covers: spira/gate-retry.sh .github/workflows/gate.yml
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
has() { case "$2" in *"$3"*) ok "$1" ;; *) bad "$1" "no [$3] in [$2]" ;; esac; }
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

# The stub batch records its argv, stdin, and SPIRA_SUITE_TIMEOUT, writes results for each
# suite named on stdin as STUB_STATUS, and exits STUB_RC.
cat > "$TMP/batch" <<'B'
#!/usr/bin/env bash
printf '%s\n' "$*" > "$STUB_LOG.args"
printf '%s\n' "${SPIRA_SUITE_TIMEOUT:-unset}" > "$STUB_LOG.timeout"
cat > "$STUB_LOG.stdin"
mkdir -p "$SPIRA_BATCH_RESULTS/k"
while read -r s; do [ -n "$s" ] && printf '%s 1 1 - serial explicit\n' "$STUB_STATUS" > "$SPIRA_BATCH_RESULTS/k/$s.result"; done < "$STUB_LOG.stdin"
exit "$STUB_RC"
B
chmod +x "$TMP/batch"

first() {  # first <dir> <suite>=<status>...
    local d="$1"; shift; rm -rf "$d" "$d-retry"; mkdir -p "$d/key"
    for kv in "$@"; do printf '%s 1 1 - parallel explicit\n' "${kv#*=}" > "$d/key/${kv%%=*}.result"; done
}
run() { GATE_RETRY_BATCH="$TMP/batch" STUB_LOG="$TMP/log" STUB_STATUS="$1" STUB_RC="$2" \
        bash "$HERE/gate-retry.sh" "$TMP/r" deadbeef > "$TMP/out" 2>&1; }

echo "test-gate-retry.sh"
first "$TMP/r" a.sh=ok b.sh=red c.sh=timeout d.sh=skip
run ok 0; rc=$?
is  "red then green passes"                       "0" "$rc"
is  "only the red and timed-out suites are re-run" "b.sh c.sh" "$(tr '\n' ' ' < "$TMP/log.stdin" | sed 's/ $//')"
has "serially"                                    "$(cat "$TMP/log.args")" "--mode serial"
has "against the same revision"                   "$(cat "$TMP/log.args")" "deadbeef"
has "each flake is announced to CI"               "$(cat "$TMP/out")" "::warning title=flaky suite::b.sh"

first "$TMP/r" a.sh=ok b.sh=red
run red 1; rc=$?
is  "red twice fails"                             "1" "$rc"
has "and names what stayed red"                   "$(cat "$TMP/out")" "red twice: b.sh"
has "and emits a red-twice annotation for CI"     "$(cat "$TMP/out")" "::error title=red-twice suite::b.sh"

first "$TMP/r" b.sh=red
run ok 3; rc=$?
is  "a harness fault on the re-run passes through" "3" "$rc"

first "$TMP/r" a.sh=ok
rm -f "$TMP/log.args"
run ok 0; rc=$?
is  "no red suite recorded: fail without retrying" "1" "$rc"
is  "and the batch is never called"               "no" "$([ -f "$TMP/log.args" ] && echo yes || echo no)"

# Timeout suites: the serial re-run uses the longer GATE_RETRY_RERUN_TIMEOUT cap (default 1200 s),
# not the original 600 s, so a suite that timed out once gets a fair chance on a serial pass.
first "$TMP/r" a.sh=ok b.sh=timeout
run ok 0; rc=$?
is  "timeout then green passes"                              "0" "$rc"
is  "serial re-run uses the longer cap (1200 s by default)" "1200" "$(cat "$TMP/log.timeout")"

echo
tl_summary
