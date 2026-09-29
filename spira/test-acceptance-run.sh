#!/usr/bin/env bash
# tier: T2
# covers: spira/acceptance-run.sh spira/acceptance-agent.sh UC-instance-lifecycle-46 UC-instance-lifecycle-47
#
# This suite verifies the MECHANISM, not the runtime result — the runtime result
# requires a clean machine with real bd/dolt/gh/claude and is what the operator
# confirms (spira/acceptance-ci.sh, acceptance.yml). The phase structure itself —
# bead-id extraction, phase env, the ready.sh rc-capture idiom, binary/tarball
# checks — is extracted into spira/acceptance-lib.sh and unit-tested directly in
# test-acceptance-lib.sh.
#
# THE TEXTUAL INVARIANTS LIVE IN spira-lint (sp-l8gl3). Everything this suite used to
# grep for — the verdict line, phase labels, waiver wiring, budgets, _ci_deploy_env on
# every deploy/uninstall/world/doctor call, --allow-draft on every deploy of the release
# under test, the aged-install override key being one conf.sh honours — is the
# `acceptance-run` rule, run by the gate's lint step on every branch without a container.
# What stays here is what needs a process: the script's argument handling, and the
# acceptance agent driven for real against a scratch repository.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"
SCRIPT="$HERE/acceptance-run.sh"
AGENT="$HERE/acceptance-agent.sh"
echo "test-acceptance-run.sh"

echo
echo "1. Usage errors exit non-zero without running a phase"

bash "$SCRIPT" >/dev/null 2>&1 && bad "no-arg invocation exits non-zero" "exit 0" \
    || ok "no-arg invocation exits non-zero"
bash "$SCRIPT" some-tag >/dev/null 2>&1 && bad "missing --scratch-repo exits non-zero" "exit 0" \
    || ok "missing --scratch-repo exits non-zero"

echo
echo "2. Phase B: a failing install.sh's output reaches the report"
SCRATCH="$(mktemp -d)"
trap 'rm -rf "$SCRATCH"' EXIT INT TERM
mkdir -p "$SCRATCH/clone"
printf '#!/bin/sh\nprintf "prev-install-failure-reason\\n"\nexit 1\n' \
    > "$SCRATCH/clone/install.sh"
chmod +x "$SCRATCH/clone/install.sh"
_pb_out="$(
    _prev_install_rc=0
    SPIRA_OPERATED=0 bash "$SCRATCH/clone/install.sh" 2>&1 | tee "$SCRATCH/prev-install.log" \
        || _prev_install_rc=$?
    [ "$_prev_install_rc" -ne 0 ] && \
        printf '  FAIL  phase B: install.sh exits 0: exit %d\n' "$_prev_install_rc"
)"
want "fixture: a failing phase B install.sh's output reaches the report" \
    "prev-install-failure-reason" "$_pb_out"

echo
echo "3. Phase B: the deploy guard names the database service"
_pg_out="$(
    _prev_install_rc=1
    [ "$_prev_install_rc" -ne 0 ] && \
        printf '  FAIL  phase B+C: skipped — install failed; database service not started: prev_install_rc=%d\n' \
            "$_prev_install_rc"
)"
want "fixture: guard names database-not-started when install fails" \
    "database service not started" "$_pg_out"

echo
echo "4. acceptance-agent.sh drives the deterministic aeon path"
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
