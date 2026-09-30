#!/usr/bin/env bash
# test-queue-flush.sh — queue.sh flush opens a batch without waiting.
#
# flush runs the batch builder with the wait threshold at zero, for the named repository or
# the home one, and refuses a repository that is not in queue mode. step settles the open
# batch before building the next, and landing's queue pass goes through it — a batch builder
# with no verdict after it opens one pull request and never lands it.
#
# covers: queue/src/* spira/verdict.sh landing-pass/src/*
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
# The queue binary (queue/DESIGN.md §7.4), invoked by name: the tree under test's build is
# on the suite's PATH (sp-gypjk).

echo "test-queue-flush.sh"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

# A copy of the harness with the round cutter and verdict replaced by ones that report how
# they were called. batch.sh's own pre-cut sweep is retired (sp-uwhx0, queue.forge has no
# live repo) — queue's flush/step call the batcher directly now, by name on PATH (real.rs's
# RealLib::context always resolves batcher_bin to the bare name "batcher").
cp -r "$HERE" "$TMP/spira"
cat > "$TMP/spira/batcher" <<'FAKE'
#!/usr/bin/env bash
printf 'batcher-called verb=%s repo=%s wait=%s\n' "${1:-}" "${2:-}" "${SPIRA_QUEUE_BATCH_WAIT:-unset}"
FAKE
chmod +x "$TMP/spira/batcher"
cat > "$TMP/spira/verdict.sh" <<'FAKE'
#!/usr/bin/env bash
printf 'verdict-called repo=%s\n' "${1:-}"
FAKE
mkdir -p "$TMP/bin" "$TMP/run"
git init -q -b main "$TMP/repo"
RMAP="$TMP/repo-map"

run() {
    env -i PATH="$TMP/bin:$TMP/spira:$PATH" HOME="$TMP" \
        SPIRA_CONF=/nonexistent SPIRA_RUN="$TMP/run" \
        SPIRA_REPO_MAP="$RMAP" SPIRA_QUEUE_BATCH_WAIT=1800 \
        SPIRA_HOME="$TMP/spira" queue "$@" 2>&1
}

echo
echo "positive control — the fake batcher is reachable and sees the configured wait:"
printf 'fixq | %s | queue | main | | |\n' "$TMP/repo" > "$RMAP"
out="$(env -i PATH=/usr/bin:/bin SPIRA_QUEUE_BATCH_WAIT=1800 "$TMP/spira/batcher" cut fixq)"
want "fake batcher reports the inherited wait" "wait=1800" "$out"

echo
echo "flush on a queue-mode repo cuts with no wait, no batch.sh sweep first:"
out="$(run flush fixq)"
want   "batcher called for the repo" "batcher-called verb=cut repo=fixq" "$out"
want   "wait threshold is zero"      "wait=0" "$out"
nowant "no separate sweep step"      "batch-called" "$out"

echo
echo "flush refuses a push-mode repo:"
printf 'fixp | %s | push | main | | |\n' "$TMP/repo" > "$RMAP"
out="$(run flush fixp)"; rc=$?
want   "names queue mode"      "not in queue mode" "$out"
nowant "batcher not called"    "batcher-called" "$out"

echo
echo "flush refuses an unknown repo:"
out="$(run flush nosuch)"
want   "names the repo"        "no such repo" "$out"
nowant "batcher not called"    "batcher-called" "$out"

echo
echo "step settles the open batch, then cuts the next — verdict before the batcher:"
printf 'fixq | %s | queue | main | | |\n' "$TMP/repo" > "$RMAP"
out="$(run step fixq)"
want "verdict called for the repo" "verdict-called repo=fixq" "$out"
want "batcher called for the repo" "batcher-called verb=cut repo=fixq" "$out"
first="$(printf '%s\n' "$out" | grep -m1 -oE '^(verdict|batcher)-called')"
is_first="${first:-none}"
want "verdict runs before the batcher" "verdict-called" "$is_first"

# RETIRED with landing.sh: the landing pass's queue step is landing-pass's Tools::queue_step
# (`queue step <repo>`), pinned by cargo test -p landing-pass
# the_queue_step_runs_before_and_after_the_walk_and_a_missing_binary_is_said.

echo
tl_summary
