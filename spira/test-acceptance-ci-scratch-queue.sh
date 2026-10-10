#!/usr/bin/env bash
#
# test-acceptance-ci-scratch-queue.sh — sp-hph6f: acceptance-ci.sh's queue.local scratch
# repo must get a LOCAL base ref, never a remote-tracking one.
#
# THE DEFECT. A fresh install's acceptance phase B starts spira-publish-prod.service, which
# runs `queue publish-settle` over every mapped repo. acceptance-ci.sh (and
# acceptance-local.sh, which mirrors it) write "origin/main" as the base column for EVERY
# scratch repo, including the one it stands up in queue.local mode — but queue.local's base
# must be a plain local branch (queue/src/ops/transition.rs's to-local creates
# `local/<branch>`, never a remote-tracking ref; spira_publish_forge resolves the forge
# target, origin/main, separately, by name — see spira-config/src/repos.rs). `queue.sh
# publish` refuses a remote-tracking base outright (queue/src/ops/publish.rs): "... resolves
# to a remote-tracking ref (origin/main) — not a queue.local base". Every real install's
# queue.local row carries `local/main` (queue/src/real.rs's own fixture asserts exactly
# that), so acceptance's own scratch repo was the one row never shaped like production.
#
# THIS SUITE runs the real acceptance-ci.sh (stubbing only `release acceptance` itself,
# which is covered on its own in release/src/acceptance/tests.rs) and then runs the real
# `queue` binary's publish-settle against the scratch-queue row it wrote — proving the
# fix end to end, not just that some string in the map file changed.
#
# tier: T0
# covers: spira/acceptance-ci.sh spira/acceptance-local.sh queue/src/ops/publish.rs
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"
SCRIPT="$HERE/acceptance-ci.sh"

echo "test-acceptance-ci-scratch-queue.sh"

export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM

# ===========================================================================
echo
echo "1. Run acceptance-ci.sh for real, stubbing only \`release acceptance\` itself"
# ===========================================================================
# A fake HOME so this never touches the real operator's ~/.config/spira or ~/scratch-*, and
# a fake notes repo (never a real git repo) so the notes-fetch/push plumbing — all of it
# gated on GH_TOKEN, which is deliberately unset here — never touches this worktree's own
# git config.
ACC_HOME="$TMP/home"; mkdir -p "$ACC_HOME"
NOTES="$TMP/notes"; mkdir -p "$NOTES"
STUB_BIN="$TMP/stub-release/release"
mkdir -p "$(dirname "$STUB_BIN")"
cat > "$STUB_BIN" <<'EOF'
#!/usr/bin/env bash
# Stands in for the candidate's own `release acceptance` (covered on its own by
# release/src/acceptance/tests.rs) — this suite is about acceptance-ci.sh's OWN scratch
# setup, which runs before this binary is ever invoked.
exit 0
EOF
chmod +x "$STUB_BIN"

_out="$TMP/acc-ci.out"
HOME="$ACC_HOME" SPIRA_RELEASE_BIN="$STUB_BIN" SPIRA_NOTES_REPO="$NOTES" \
    bash "$SCRIPT" test-tag --bd-db "$ACC_HOME/bd-db" >"$_out" 2>&1
wantrc "acceptance-ci.sh exits 0 (stub release succeeded)" "0" "$?"

# Found by directory, never by its bare filename — spira-config (and its CLI) is the only
# thing allowed to NAME that file (config-fence.sh); every other reader goes through it.
MAP="$(find "$ACC_HOME/.config/spira" -maxdepth 1 -type f 2>/dev/null | head -1)"
if [ -n "$MAP" ] && [ -f "$MAP" ]; then
    ok "acceptance-ci.sh wrote its scratch map"
else
    bad "acceptance-ci.sh wrote its scratch map" "no file under $ACC_HOME/.config/spira ($(cat "$_out"))"
    tl_summary
fi

# ===========================================================================
echo
echo "2. The queue.local row's base is a local branch, not origin/main"
# ===========================================================================

Q_ROW="$(awk -F'\\|' '{n=$1; gsub(/^[ \t]+|[ \t]+$/,"",n); if (n=="scratch-queue") print}' "$MAP")"
want "positive control: the map has a scratch-queue row at all" "scratch-queue" "$Q_ROW"
nowant "scratch-queue's base is not origin/main (the defect)" "| origin/main |" "$Q_ROW"
want "scratch-queue's base is the local/main branch" "| local/main |" "$Q_ROW"

# REGRESSION — the push-mode and pr-mode rows are untouched: their base is still the
# remote-tracking ref they always had, since only queue.local needs a local one.
P_ROW="$(awk -F'\\|' '{n=$1; gsub(/^[ \t]+|[ \t]+$/,"",n); if (n=="scratch-repo") print}' "$MAP")"
want "scratch-repo (push mode) still has origin/main as base (regression)" "| origin/main |" "$P_ROW"
PR_ROW="$(awk -F'\\|' '{n=$1; gsub(/^[ \t]+|[ \t]+$/,"",n); if (n=="scratch-pr") print}' "$MAP")"
want "scratch-pr (pr mode) still has origin/main as base (regression)" "| origin/main |" "$PR_ROW"

# The local/main branch must actually exist in the scratch-queue checkout (a correct row
# pointing at a branch that was never created would just move the failure).
if git -C "$ACC_HOME/scratch-queue" rev-parse --verify -q refs/heads/local/main >/dev/null 2>&1; then
    ok "local/main exists as a real local branch in scratch-queue"
else
    bad "local/main exists as a real local branch in scratch-queue" "no such ref"
fi

# ===========================================================================
echo
echo "3. END TO END: the real \`queue\` binary's publish-settle against this row"
# ===========================================================================
# Mirrors test-queue-publish.sh's own env wiring (sp-bc49w) — the CONTEXT seam still
# shells to bash with SPIRA_HOME sourcing lib.sh/conf.sh for Settings; the repo-specific
# fields (mode/base/landref/publish) are resolved in-process from SPIRA_REPO_MAP.
SH="$TMP/spira"; mkdir -p "$SH"
cp -r "$HERE"/*.sh "$HERE"/*.py "$SH/" 2>/dev/null || true
cp -r "$HERE/conf.d" "$HERE/conf-gen.sh" "$SH/"
ln -sf "$(command -v mail)" "$SH/mail" 2>/dev/null || true
chmod +x "$SH"/*.sh 2>/dev/null || true

RUN="$TMP/run"; mkdir -p "$RUN/worktree" "$RUN/queue"

queue_cmd() {
    tl_config SPIRA_HOME_REPO=scratch-queue SPIRA_RUN="$RUN" SPIRA_MAIL="$RUN/mail" \
        SPIRA_QUEUE_DIR="$RUN/queue" SPIRA_REPO_MAP="$MAP" \
        SPIRA_FORGE="$SH/forge-missing.sh" SPIRA_RELEASES="$TMP/releases"
    SPIRA_CONF=/nonexistent \
    SPIRA_HOME="$SH" \
    SPIRA_REPO="$ACC_HOME/scratch-queue" \
        command queue "$@" 2>&1
}

_pub_out="$(queue_cmd publish-settle scratch-queue)"
_pub_rc=$?

nowant "publish-settle never reports scratch-queue as a bad queue.local base" \
    "not a queue.local base" "$_pub_out"
is "publish-settle has nothing to publish (local/main == origin/main, both at init)" \
    "0" "$_pub_rc"
want "publish-settle's own verdict names the no-op outcome" "nothing to publish" "$_pub_out"

tl_summary
