#!/usr/bin/env bash
# test-queue-flush.sh — queue.sh flush opens a batch without waiting.
#
# flush runs the batch builder with the wait threshold at zero, for the named repository or
# the home one, and refuses a repository that is not in queue mode. step settles the open
# batch before building the next, and landing's queue pass goes through it — a batch builder
# with no verdict after it opens one pull request and never lands it.
#
# covers: spira/queue.sh spira/batch.sh spira/verdict.sh spira/landing.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

echo "test-queue-flush.sh"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

# A copy of the harness with the batch builder replaced by one that reports how it was called.
cp -r "$HERE" "$TMP/spira"
cat > "$TMP/spira/batch.sh" <<'FAKE'
#!/usr/bin/env bash
printf 'batch-called repo=%s wait=%s\n' "${1:-}" "${SPIRA_QUEUE_BATCH_WAIT:-unset}"
FAKE
cat > "$TMP/spira/verdict.sh" <<'FAKE'
#!/usr/bin/env bash
printf 'verdict-called repo=%s\n' "${1:-}"
FAKE
mkdir -p "$TMP/bin" "$TMP/run"
git init -q -b main "$TMP/repo"
RMAP="$TMP/repo-map"

run() {
    env -i PATH="/usr/local/bin:/usr/bin:/bin" HOME="$TMP" \
        SPIRA_CONF=/nonexistent SPIRA_PATH="$TMP/bin" SPIRA_RUN="$TMP/run" \
        SPIRA_REPO_MAP="$RMAP" SPIRA_QUEUE_BATCH_WAIT=1800 \
        bash "$TMP/spira/queue.sh" "$@" 2>&1
}

echo
echo "positive control — the fake builder is reachable and sees the configured wait:"
printf 'fixq | %s | queue | main | | |\n' "$TMP/repo" > "$RMAP"
out="$(env -i PATH=/usr/bin:/bin SPIRA_QUEUE_BATCH_WAIT=1800 bash "$TMP/spira/batch.sh" fixq)"
want "fake builder reports the inherited wait" "wait=1800" "$out"

echo
echo "flush on a queue-mode repo runs the builder with no wait:"
out="$(run flush fixq)"
want "builder called for the repo" "batch-called repo=fixq" "$out"
want "wait threshold is zero"      "wait=0" "$out"

echo
echo "flush refuses a push-mode repo:"
printf 'fixp | %s | push | main | | |\n' "$TMP/repo" > "$RMAP"
out="$(run flush fixp)"; rc=$?
want   "names queue mode"      "not in queue mode" "$out"
nowant "builder not called"    "batch-called" "$out"

echo
echo "flush refuses an unknown repo:"
out="$(run flush nosuch)"
want   "names the repo"        "no such repo" "$out"
nowant "builder not called"    "batch-called" "$out"

echo
echo "step settles the open batch, then builds the next:"
printf 'fixq | %s | queue | main | | |\n' "$TMP/repo" > "$RMAP"
out="$(run step fixq)"
want "verdict called for the repo" "verdict-called repo=fixq" "$out"
want "batch called for the repo"   "batch-called repo=fixq" "$out"
first="$(printf '%s\n' "$out" | grep -m1 -oE '^(verdict|batch)-called')"
is_first="${first:-none}"
want "verdict runs before batch"   "verdict-called" "$is_first"

echo
echo "landing's queue pass runs step, not the batch builder alone:"
lq="$(grep -A3 '= queue \] || continue' "$HERE/landing.sh" | grep -E 'queue\.sh|batch\.sh' | head -1)"
want   "landing calls queue.sh step" 'queue.sh" step' "$lq"
nowant "landing does not call batch.sh directly" 'batch.sh' "$lq"

echo
echo "test-queue-flush.sh: $pass passed, $fail failed"
[ "$fail" -eq 0 ]
