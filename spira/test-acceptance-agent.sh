#!/usr/bin/env bash
# tier: T2
# covers: spira/acceptance-agent.sh
#
# acceptance-agent.sh — the stub that replaces claude in acceptance — driven for real
# against a scratch repository. The acceptance run itself is `release acceptance`
# (sp-ak7qm), unit-tested in release/src/acceptance/tests.rs; this suite was
# test-acceptance-run.sh, whose usage and phase-B fixtures moved there with it.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"
AGENT=acceptance-agent.sh   # the SUT, by name on the suite's PATH (sp-gypjk)
echo "test-acceptance-agent.sh"

echo
echo "1. acceptance-agent.sh drives the deterministic aeon path"
# THE STUB MUST COMMIT FOR EVERY BEAD, NOT ONCE PER SCRATCH REPO. It wrote the same empty
# acceptance-probe.txt every time; phase A committed it to the scratch repo's main, so the
# phase-D bead (same repo, surviving state) found "nothing to commit", closed with no
# commit, was converted to submitted and never landed (stage 3: "no commit naming bead id
# on branch after 60s", 2026-09-26). Driven for real, twice, the second time on a branch
# that already carries the first probe — exactly phase D's shape.
_ag_tmp="$(mktemp -d)"
git init -q -b main "$_ag_tmp/repo"
git -C "$_ag_tmp/repo" -c user.email=t@t -c user.name=t commit -q --allow-empty -m init
printf '#!/usr/bin/env bash\nexit 0\n' > "$_ag_tmp/bd"; chmod +x "$_ag_tmp/bd"
_ag_run() {   # _ag_run <bead-id>
    ( cd "$_ag_tmp/repo" && env -i PATH="$PATH" HOME="$_ag_tmp" SPIRA_CONF=/nonexistent \
        SPIRA_BD="$_ag_tmp/bd" SPIRA_DB="$_ag_tmp/db" SPIRA_RUN="$_ag_tmp/run" BEAD_ID="$1" \
        GIT_AUTHOR_NAME=a GIT_AUTHOR_EMAIL=a@a GIT_COMMITTER_NAME=a GIT_COMMITTER_EMAIL=a@a \
        bash "$AGENT" </dev/null >/dev/null 2>&1 )
}
_ag_run sp-agt1
is "acceptance-agent.sh commits for the first bead" "sp-agt1: acceptance probe" \
   "$(git -C "$_ag_tmp/repo" log -1 --format=%s 2>/dev/null)"
git -C "$_ag_tmp/repo" checkout -q -b spira/sp-agt2
_ag_run sp-agt2
is "and again for a second bead on a branch that already carries the first probe" \
   "sp-agt2: acceptance probe" "$(git -C "$_ag_tmp/repo" log -1 --format=%s 2>/dev/null)"
# A SWEEP SESSION HAS NO BEAD. Ops and the other sweep personas summon the agent with no
# BEAD_ID; the stub exited 1 ("BEAD_ID not set"), spira-ops was left FAILED, and every later
# deploy's pre-health check refused on it (local phases B and D, 2026-09-26). A sweep has
# nothing to commit or close: the stub reports a finished turn and exits 0.
_sw_out="$( cd "$_ag_tmp/repo" && env -i PATH="$PATH" HOME="$_ag_tmp" SPIRA_CONF=/nonexistent \
    SPIRA_BD="$_ag_tmp/bd" SPIRA_DB="$_ag_tmp/db" SPIRA_RUN="$_ag_tmp/run" \
    bash "$AGENT" </dev/null 2>&1 )"; _sw_rc=$?
is   "acceptance-agent.sh: a sweep session (no BEAD_ID) exits 0" 0 "$_sw_rc"
want "and reports a finished turn"                             '"type":"result"' "$_sw_out"
rm -rf "$_ag_tmp"; unset _ag_tmp _sw_out _sw_rc

tl_summary
