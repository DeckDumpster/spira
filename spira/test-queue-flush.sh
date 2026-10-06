#!/usr/bin/env bash
# test-queue-flush.sh — queue.sh flush opens a batch without waiting.
#
# flush runs the batch builder with the wait threshold at zero, for the named repository or
# the home one, and refuses a repository that is not in queue mode. step settles the open
# batch before building the next, and landing's queue pass goes through it — a batch builder
# with no verdict after it opens one pull request and never lands it.
#
# tier: T1
# covers: queue/src/* landing-pass/src/* spira/lib.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
. "$HERE/testlib/lc-fixture.sh"

echo "test-queue-flush.sh"
TMP="$(mktemp -d)"; trap 'lcfix_down; rm -rf "$TMP"' EXIT INT TERM
lcfix_up || { echo "test-queue-flush: could not build a lifecycle fixture"; exit 1; }

# A copy of the harness with the round cutter replaced by one that reports how it was
# called. batch.sh's own pre-cut sweep is retired (sp-uwhx0, queue.forge has no live repo)
# — queue's flush/step call the batcher directly now, by name on PATH (real.rs's
# RealLib::context always resolves batcher_bin to the bare name "batcher").
cp -r "$HERE" "$TMP/spira"
# The verdict runs inside `queue step` now (queue/DESIGN-verdict.md); its first act on an
# open batch is the forge's check-status, so a forge that logs that call — and the batcher
# logging its own — is what shows the real order the subprocess calls happened in.
ORDER="$TMP/order"
cat > "$TMP/spira/batcher" <<FAKE
#!/usr/bin/env bash
printf 'batcher-called verb=%s repo=%s wait=%s\n' "\${1:-}" "\${2:-}" "\${SPIRA_QUEUE_BATCH_WAIT:-unset}"
printf 'batcher-called\n' >> "$ORDER"
FAKE
chmod +x "$TMP/spira/batcher"
cat > "$TMP/spira/forge-fake.sh" <<FAKE
#!/usr/bin/env bash
[ "\${1:-}" = check-status ] && printf 'verdict-called\n' >> "$ORDER"
printf 'pending\n'
FAKE
chmod +x "$TMP/spira/forge-fake.sh"
mkdir -p "$TMP/bin" "$TMP/run"
git init -q -b main "$TMP/repo"
git -C "$TMP/repo" -c user.name=t -c user.email=t@t commit -q --allow-empty -m base
RMAP="$TMP/repo-map"

run() {
    # The complete fixture declares batcher_enable=0 (the operator cuts rounds); this
    # suite is specifically about the batcher doing its own cut, so it must say so
    # (sfail round 3, pattern 7).
    # SPIRA_QUEUE_DIR is registered too: its step lock lives under it, and the complete
    # fixture's own bogus default doesn't exist (sfail round 4, pattern 7).
    tl_config SPIRA_RUN="$TMP/run" SPIRA_REPO_MAP="$RMAP" SPIRA_QUEUE_BATCH_WAIT=1800 \
        SPIRA_FORGE="$TMP/spira/forge-fake.sh" SPIRA_BATCHER_ENABLE=1 \
        SPIRA_QUEUE_DIR="$TMP/run/queue"
    env -i SPIRA_TOML="$SPIRA_TOML" $(lcfix_env) PATH="$TMP/bin:$TMP/spira:$PATH" HOME="$TMP" \
        SPIRA_CONF=/nonexistent \
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
mkdir -p "$TMP/run/queue/fixq"
printf 'pr=1\nhead=h\nbase=b\nmembers=\nopened=1\nbranch=spira/queue/x\n' > "$TMP/run/queue/fixq/open"
: > "$ORDER"
out="$(run step fixq)"
want "verdict settled the open batch (pending)" "verdict fixq: PR 1 pending" "$out"
want "batcher called for the repo" "batcher-called verb=cut repo=fixq" "$out"
is "verdict runs before the batcher" "verdict-called batcher-called" "$(tr '\n' ' ' < "$ORDER" | sed 's/ $//')"

# RETIRED with landing.sh: the landing pass's queue step is landing-pass's Tools::queue_step
# (`queue step <repo>`), pinned by cargo test -p landing-pass
# the_queue_step_runs_before_and_after_the_walk_and_a_missing_binary_is_said.

echo
tl_summary
