#!/usr/bin/env bash
#
# test-gate-workflow.sh — the CI gate workflow runs the real gate, and a release
# is cut only from a gate that actually ran something.
#
#   ./test-gate-workflow.sh
#
# WHY A WORKFLOW NEEDS A SUITE. A GitHub workflow is executed by a machine nobody
# here owns, on a push nobody here watches, and it reports its own success. Every
# failure mode this harness has learned about self-reporting checks applies to it
# with less visibility, not more. These are the properties that cannot be allowed
# to drift silently.
#
#   1. THE GATE IS THE REAL GATE. The three lints and testenv-batch.sh, the same
#      command the landing gate runs locally. A CI job that runs a cheaper subset
#      is a green that does not mean what the local green means.
#
#   2. A PUSH TO THE BASE BRANCH RUNS THE WHOLE CORPUS. testenv-batch.sh selects
#      suites from the diff against the base ref. On a push to main that diff is
#      EMPTY, so diff-derived selection would select nothing, pass, and cut a
#      release off a gate that ran zero suites. The release gate must therefore
#      name the corpus explicitly (law-a-runner-takes-a-list).
#
#   3. AN INFRASTRUCTURE FAULT IS NOT A BRANCH FAILURE. testenv-batch exits 2 or 3
#      when the container never came up or the install failed. The repo-map's gate
#      command maps those to 75 for exactly this reason: the branch was never
#      tested, so it must not be reported as tested-and-bad.
#
#   4. THE RELEASE IS GATED, AND ONLY ON THE BASE BRANCH. A release job that does
#      not depend on the gate job publishes untested code; one that runs on pull
#      requests publishes from a branch that never landed.
#
#   5. THE TAG PUSH CANNOT RELY ON A TRIGGER THAT WILL NOT FIRE. GitHub does not
#      run workflows for a tag pushed with GITHUB_TOKEN — a deliberate anti-
#      recursion rule. A gate that cuts a tag and trusts release.yml's tag trigger
#      to publish it produces a tag and no release, silently, forever. The publish
#      must be invoked directly.
#
#   6. HISTORY MUST BE FETCHED. release.sh reads the beads landed since the
#      previous release tag; actions/checkout defaults to depth 1 and fetches no
#      tags, which makes every cut look like the first one.
#
# covers: .github/workflows/gate.yml .github/workflows/acceptance.yml .github/workflows/release.yml
# covers: .github/workflows/testenv-image.yml spira/test-fixtures/ephemeral-ci-v1
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
ROOT="$(cd "$HERE/.." && pwd -P)"
pass=0; fail=0
ok()   { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()  { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "${2:-}"; }
want() { case "$3" in *"$2"*) ok "$1" ;; *) bad "$1" "wanted [$2]" ;; esac; }
nowant() { case "$3" in *"$2"*) bad "$1" "must not contain [$2]" ;; *) ok "$1" ;; esac; }

echo "test-gate-workflow.sh"

GATE_YML="$ROOT/.github/workflows/gate.yml"
REL_YML="$ROOT/.github/workflows/release.yml"

echo
echo "the workflow exists:"
if [ -r "$GATE_YML" ]; then
    ok "gate.yml exists at .github/workflows/gate.yml"
else
    bad "gate.yml exists at .github/workflows/gate.yml" "not found"
    printf '\n  %d passed, %d failed\n' "$pass" "$fail"; exit 1
fi
G="$(cat "$GATE_YML")"

# POSITIVE CONTROL. Confirm the file was read and these are substring matches over
# real content before trusting anything absent from it.
want "positive control: the file has a name" "name:" "$G"

echo
echo "1. it runs the real gate, not a cheaper subset:"
want "inventory.sh"      "spira/inventory.sh"    "$G"
want "literal-lint.sh"   "spira/literal-lint.sh" "$G"
want "scratch-fence.sh"  "spira/scratch-fence.sh" "$G"
want "testenv-batch.sh"  "spira/testenv-batch.sh" "$G"

echo
echo "2. queue PRs use diff-selected suites via select.sh (law-a-runner-takes-a-list):"
# Queue PRs run the diff-selected suites: select.sh derives the suite list from
# the batch diff and pipes it to testenv-batch.sh --suites -. Inert files
# (*.md etc.) are filtered; genuinely unmapped source still triggers all-suites.
want "the selected list is piped via --suites" "--suites"      "$G"
want "queue PRs use select.sh"                 "select.sh"    "$G"
want "queue PRs match spira/queue/"            'spira/queue/' "$G"

echo
echo "3. an infrastructure fault is distinguished from a branch failure:"
want "the harness-fault exit code is handled" "75" "$G"

echo
echo "4. the release is gated, and only on the base branch:"
want "a job depends on the gate"        "needs:"            "$G"
want "release.sh cut is what tags"      "release.sh"        "$G"
want "restricted to push events"        "github.event_name" "$G"
want "restricted to the base branch"    "refs/heads/main"   "$G"

echo
echo "5. publishing does not rely on the tag trigger:"
# The tag push happens, but the publish is invoked directly because a
# GITHUB_TOKEN tag push fires no workflow.
want "release.yml is invoked directly"  "uses: ./.github/workflows/release.yml" "$G"
want "release.yml accepts being called" "workflow_call"     "$(cat "$REL_YML")"

echo
echo "6. history and tags are fetched:"
want "fetch-depth is set"  "fetch-depth" "$G"
want "tags are fetched"    "fetch-tags"  "$G"

echo
echo "7. the runner is provisioned per run, not a standing label:"
# There is NO standing pool of general-purpose self-hosted runners. A runner
# exists only because a provision job created it, and its label is unique to that
# run. A job naming a fixed label waits for a runner that is never registered:
# it queues forever, reports nothing, and looks exactly like a busy queue.
want "the VM is provisioned"        "ephemeral-ci/provision@v1"                        "$G"
want "the gate targets that VM"     'needs.provision.outputs.label'                    "$G"
nowant "no fixed runner label"      "self-hosted, linux, x64"                          "$G"

echo
echo "8. the VM is destroyed whatever the outcome:"
# if: always() is load-bearing. A cancelled or failed run otherwise leaves the VM
# alive and the hourly reaper becomes the only thing that cleans up, which is a
# backstop and not a plan. The gate cancels superseded pull-request runs by
# design, so this is the common case here, not the rare one.
want "teardown runs"                "ephemeral-ci/teardown@v1"                         "$G"
want "teardown is unconditional"    "always()"                                         "$G"

echo
echo "9. the gate confirms which machine it landed on:"
# A label collision or a stale registration would run the gate somewhere else
# entirely, and every step would still report success. RUNNER_NAME is set by the
# runner itself, so it is the one value the workflow cannot assert into being true.
want "the runner identity is checked" "RUNNER_NAME"                                    "$G"

echo
echo "10. the VM is given what the suites need before they run:"
# A cold VM has no podman, no image and no caches. testenv-batch.sh exits 2 when
# the container does not come up, which this workflow maps to 75 -- so a missing
# dependency reports as a harness fault forever rather than as the one-line fix
# it is. The dependency list is executable and lives in the repository.
want "host dependencies are installed" "runner-deps.sh"                                "$G"

echo
echo "11. the gate acquires the test image rather than building it:"
# The image is ~1.8 GB and its build downloads a Go toolchain, compiles bd from
# source and installs a Rust toolchain. A machine created for one run has no
# layer cache, so an unconfigured gate pays that build on every single run and it
# dominates the wall clock. Pointing SPIRA_TESTENV_REGISTRY at a registry turns
# that build into a pull; testenv.sh falls back to building on a miss, so this is
# a cost control and never a correctness one.
want "the gate names a registry"     "SPIRA_TESTENV_REGISTRY" "$G"
want "the gate authenticates to it"  "podman login"           "$G"

echo
echo "12. the image is published, and only under the closure hash:"
IMG_YML="$ROOT/.github/workflows/testenv-image.yml"
if [ -r "$IMG_YML" ]; then
    ok "testenv-image.yml exists"
    I="$(cat "$IMG_YML")"
    want   "positive control: the file has a name" "name:"                  "$I"
    want   "it publishes through testenv.sh"       "testenv.sh publish"     "$I"
    # An existing tag must not be rebuilt. Without that check every push to the
    # base branch pays the full build to republish bytes that are already there.
    want   "an already-published tag is skipped"   "manifest inspect"       "$I"
    # GHCR rejects an uppercase path. The owner reaches this as-typed, so a repo
    # under a capitalised organisation fails at push time with an error that
    # names authentication rather than case.
    want   "the registry path is lowercased"       "tr '[:upper:]' '[:lower:]'" "$I"
    nowant "no floating tag is published"          ":latest"                "$I"
else
    bad "testenv-image.yml exists" "not found at .github/workflows/testenv-image.yml"
fi

echo
echo "13. two pushes to the base branch cannot cancel one another:"
# THE GROUP, NOT THE FLAG. cancel-in-progress was an expression meant to be false for a
# push, and it did not hold: two pushes were each cancelled at the instant the next
# arrived, so neither was gated and neither was cut. A release cut from a later run then
# carries those commits as though they had passed a gate they never ran.
#
# Keying the group on the commit makes two push runs structurally incapable of sharing a
# group, so cancellation is impossible regardless of how the flag coerces. A pull request
# still keys on its ref, so a superseded run is still cancelled -- which is wanted there,
# because no release is cut from it.
# SCOPED TO THE CONCURRENCY BLOCK. github.sha appears in the Suites step too, so a
# match over the whole file is satisfied by a line that has nothing to do with this and
# reports a green the workflow has not earned.
CONC="$(awk '/^concurrency:/{f=1} f{print} f&&/^[a-z]/&&!/^concurrency:/{exit}' "$GATE_YML")"
want "positive control: the concurrency block was found" "group:" "$CONC"
want "the push group is keyed on the commit"             "github.sha" "$CONC"
want "a pull request still supersedes itself"            "cancel-in-progress" "$CONC"

echo
echo "13b. a closed pull request still enters the gate-ref group, so cancel-in-progress kills its superseded run (sp-1p04d):"
# GitHub does not cancel a batch's Gate run when its PR closes on its own. Adding
# closed as a trigger type means a PR close queues a new run in the SAME group as
# the run(s) it supersedes (every pull_request event keys on github.ref), and
# cancel-in-progress is unconditionally true for pull_request events, so the new
# run cancels whatever was still going for that PR.
_pr_block="$(awk '/^  pull_request:$/{f=1;next} f&&/^  [a-z_-]+:$/{exit} f{print}' "$GATE_YML")"
if [ -z "$_pr_block" ]; then
    bad "the pull_request trigger block was located (positive control)" "awk extracted nothing"
else
    ok "the pull_request trigger block was located"
fi
want "pull_request trigger adds the closed type" "closed" "$_pr_block"
want "pull_request trigger keeps the default open/sync/reopen types" "opened" "$_pr_block"
want "the group still keys every pull_request run on the ref" "github.ref" "$CONC"

echo
echo "13c. a closed-PR run does no work — it only occupies the group to trigger cancellation:"
_select_job_block="$(awk '/^  select:$/{f=1;next} f&&/^  [a-z_-]+:$/{exit} f{print}' "$GATE_YML")"
if [ -z "$_select_job_block" ]; then
    bad "the select job block was located (positive control)" "awk extracted nothing"
else
    ok "the select job block was located"
fi
want "select refuses to run for a closed PR" "event.action != 'closed'" "$_select_job_block"

echo
echo "the suites job is bounded, because it holds a real machine:"
# Without timeout-minutes the job inherits GitHub's 360-minute default and a wedge
# holds a provisioned VM for six hours. Read from the parsed YAML rather than by
# grepping the file: a `timeout-minutes` under any OTHER job would satisfy a grep
# while the suites job stayed unbounded, which is the only case that matters.
#
# NO PyYAML. The first version of this asked python3 for the parsed document and
# the test image has no yaml module, so the import died, 2>/dev/null swallowed it,
# and the check reported "timeout unset" against a workflow that sets it -- a false
# RED carrying an actively misleading message. Scoped awk instead: take the suites
# job's block only, from `  suites:` to the next key at the same indent.
_suites_job_block="$(awk '/^  suites:$/{f=1;next} f&&/^  [a-z_-]+:$/{exit} f{print}' "$GATE_YML")"
# A block that came back empty would make the assertion below vacuous, and the
# message would again blame the workflow for the matcher's fault.
if [ -z "$_suites_job_block" ]; then
    bad "the suites job block was located (positive control)" "awk extracted nothing; the two assertions below would be vacuous"
else
    ok "the suites job block was located (positive control)"
fi
_t="$(printf '%s' "$_suites_job_block" | sed -n 's/^[[:space:]]*timeout-minutes:[[:space:]]*\([0-9][0-9]*\).*/\1/p' | head -1)"
if [ -n "$_t" ]; then
    ok "the suites job sets timeout-minutes ($_t)"
else
    bad "the suites job sets timeout-minutes" "unset; the job inherits GitHub's 360-minute default and a wedge holds a provisioned VM for six hours"
fi
# Bigger than one measured pass (24 min) and smaller than the default it replaces.
case "$_t" in
    ''|*[!0-9]*) bad "the timeout is a sane bound" "not a number: [$_t]" ;;
    *) if [ "$_t" -gt 24 ] && [ "$_t" -lt 360 ]; then
           ok "the timeout is a sane bound"
       else
           bad "the timeout is a sane bound" "$_t minutes: a full pass measured 24, and 360 is the default this exists to replace"
       fi ;;
esac

ACC_YML="$ROOT/.github/workflows/acceptance.yml"

echo
echo "14. push to main runs the whole corpus (law-a-runner-takes-a-list):"
# The select job pre-computes the suite list before any machine is provisioned.
# For push it enumerates the full corpus (test-*.sh); for queue PRs it uses
# select.sh. The suites job pipes the pre-computed list via --suites -.
_select_step="$(awk '/^      - name: Select suites/{f=1;next} f&&/^      - name:/{exit} f{print}' "$GATE_YML")"
if [ -z "$_select_step" ]; then
    bad "the Select suites step was located (positive control)" "awk extracted nothing"
else
    ok "the Select suites step was located (positive control)"
fi
want "push enumerates test-*.sh"       'test-*.sh'    "$_select_step"
want "queue PRs are matched"           'spira/queue/' "$_select_step"
want "queue selection uses select.sh"  "select.sh"    "$_select_step"
_suites_block="$(awk '/^      - name: Suites/{f=1;next} f&&/^      - name:/{exit} f{print}' "$GATE_YML")"
if [ -z "$_suites_block" ]; then
    bad "the Suites step block was located (positive control)" "awk extracted nothing"
else
    ok "the Suites step block was located (positive control)"
fi
want "suites step pipes pre-computed list to batch" '--suites -' "$_suites_block"

echo
echo "15. the cut job asserts a green gate check and non-empty suite results before tagging:"
# The push gate runs the full corpus. Assert both: a green gate check on this SHA,
# and that the batch-results artifact has .result files (law-absence-needs-a-positive-control).
_cut_block="$(awk '/^  cut:$/{f=1;next} f&&/^  [a-z_-]+:$/{exit} f{print}' "$GATE_YML")"
if [ -z "$_cut_block" ]; then
    bad "the cut job block was located (positive control)" "awk extracted nothing"
else
    ok "the cut job block was located (positive control)"
fi
want "cut queries check-runs for the SHA"  "check-runs"    "$_cut_block"
want "cut filters on the gate check name"  '"gate"'        "$_cut_block"
want "cut checks batch-results artifact"   "batch-results" "$_cut_block"
want "cut counts .result files"            ".result"       "$_cut_block"
want "release.sh cut receives the workspace" "release.sh cut" "$_cut_block"

echo
echo "16. positive control: the cut step's suite-count check catches an empty results dir:"
# Verify the find/.result counting logic that guards the cut step.
_td="$(mktemp -d)"
_count="$(find "$_td" -name '*.result' 2>/dev/null | wc -l)"
if [ "${_count:-0}" -eq 0 ]; then
    ok "empty dir has zero .result files (count check would refuse)"
else
    bad "empty dir had unexpected .result files"
fi
printf 'green 1000000000 5 abc parallel all 0\n' > "$_td/test-foo.sh.result"
_count="$(find "$_td" -name '*.result' 2>/dev/null | wc -l)"
if [ "${_count:-0}" -gt 0 ]; then
    ok "dir with one result file has count > 0 (count check would pass)"
else
    bad "dir with a .result file still counted zero"
fi
rm -rf "$_td"

echo
echo "16. a failed provision leaves the gate check failing, not skipped:"
# When provision fails the suites job is skipped (it depends on provision with no
# if:). Without a gate job that runs unconditionally, the check named 'gate' would
# conclude 'skipped'. GitHub branch protection treats skipped as passing, so the
# push is admitted. The gate job runs under !cancelled() — true for push events,
# whose concurrency group is per-SHA — and exits 75 (harness fault) when provision
# failed, so branch protection sees 'failure' and refuses the push.
_gate_verdict_block="$(awk '/^  gate:$/{f=1;next} f&&/^  [a-z_-]+:$/{exit} f{print}' "$GATE_YML")"
if [ -z "$_gate_verdict_block" ]; then
    bad "the gate verdict job block was located (positive control)" "awk extracted nothing; the assertions below would be vacuous"
else
    ok "the gate verdict job block was located (positive control)"
fi
want "gate needs provision"                 "provision"          "$_gate_verdict_block"
want "gate needs suites"                    "suites"             "$_gate_verdict_block"
want "gate needs build"                     "build"              "$_gate_verdict_block"
want "gate runs even when needs failed"     "!cancelled()"       "$_gate_verdict_block"
want "gate runs on a hosted runner"         "ubuntu-latest"      "$_gate_verdict_block"
want "gate exits 75 on provision fault"     "75"                 "$_gate_verdict_block"
want "gate checks provision.result"         "provision.result"   "$_gate_verdict_block"
want "gate checks suites.result"            "suites.result"      "$_gate_verdict_block"

echo
echo "16b. the build job compiles binaries in parallel with provision, and suites downloads them:"
# The build job must not depend on provision — that is what makes it run in
# parallel rather than adding its time to the critical path.
_build_job_block="$(awk '/^  build:$/{f=1;next} f&&/^  [a-z_-]+:$/{exit} f{print}' "$GATE_YML")"
if [ -z "$_build_job_block" ]; then
    bad "the build job block was located (positive control)" "awk extracted nothing; the assertions below would be vacuous"
else
    ok "the build job block was located (positive control)"
fi
want "build job needs select"               "needs: select"      "$_build_job_block"
nowant "build job does not need provision"  "provision"          "$_build_job_block"
want "build job calls make build"           "make build"         "$_build_job_block"
want "build job uploads the binaries artifact" "upload-artifact" "$_build_job_block"
want "suites job needs build"               "build"              "$_suites_job_block"
want "suites job downloads the binaries artifact" "download-artifact" "$_suites_job_block"

echo
echo "17. every required action input at the pinned v1 is passed (derived from the action.yml fixtures):"
# POSITIVE CONTROL is built in: after checking the real workflows we also check a
# synthetic teardown block with pve-ca-cert stripped, and assert the check catches it.
_prov_fix="$HERE/test-fixtures/ephemeral-ci-v1/provision-action.yml"
_tear_fix="$HERE/test-fixtures/ephemeral-ci-v1/teardown-action.yml"
if [ -r "$_prov_fix" ] && [ -r "$_tear_fix" ]; then
    ok "action fixtures exist"
else
    bad "action fixtures exist" "expected spira/test-fixtures/ephemeral-ci-v1/{provision,teardown}-action.yml"
fi

_parse_required() {
    # Emit each input name whose `required: true` line appears inside `inputs:`.
    awk '
        /^inputs:/           { in_inputs=1; next }
        /^(outputs|runs):/   { in_inputs=0; next }
        in_inputs && /^  [a-z][a-z0-9-]+:$/ { name=$1; sub(/:$/,"",name) }
        in_inputs && name && /^    required: true$/ { print name }
    ' "$1"
}

_check_inputs() {
    # $1 = workflow label, $2 = required-input list (newline-sep), $3 = workflow block
    local _label="$1" _block="$3"
    while IFS= read -r _inp; do
        [ -n "$_inp" ] || continue
        want "$_label passes required input: $_inp" "$_inp" "$_block"
    done <<< "$2"
}

_prov_required="$(_parse_required "$_prov_fix")"
_tear_required="$(_parse_required "$_tear_fix")"

# gate.yml
_gate_prov_block="$(awk '/^  provision:$/{f=1;next} f&&/^  [a-z_-]+:$/{exit} f{print}' "$GATE_YML")"
_gate_tear_block="$(awk '/^  teardown:$/{f=1;next} f&&/^  [a-z_-]+:$/{exit} f{print}' "$GATE_YML")"
if [ -z "$_gate_prov_block" ]; then
    bad "gate.yml provision block found (positive control)" "awk extracted nothing"
else
    ok "gate.yml provision block found"
    _check_inputs "gate.yml provision" "$_prov_required" "$_gate_prov_block"
fi
if [ -z "$_gate_tear_block" ]; then
    bad "gate.yml teardown block found (positive control)" "awk extracted nothing"
else
    ok "gate.yml teardown block found"
    _check_inputs "gate.yml teardown" "$_tear_required" "$_gate_tear_block"
fi

# acceptance.yml
if [ -r "$ACC_YML" ]; then
    _acc_prov_block="$(awk '/^  provision:$/{f=1;next} f&&/^  [a-z_-]+:$/{exit} f{print}' "$ACC_YML")"
    _acc_tear_block="$(awk '/^  teardown:$/{f=1;next} f&&/^  [a-z_-]+:$/{exit} f{print}' "$ACC_YML")"
    if [ -z "$_acc_prov_block" ]; then
        bad "acceptance.yml provision block found (positive control)" "awk extracted nothing"
    else
        ok "acceptance.yml provision block found"
        _check_inputs "acceptance.yml provision" "$_prov_required" "$_acc_prov_block"
    fi
    if [ -z "$_acc_tear_block" ]; then
        bad "acceptance.yml teardown block found (positive control)" "awk extracted nothing"
    else
        ok "acceptance.yml teardown block found"
        _check_inputs "acceptance.yml teardown" "$_tear_required" "$_acc_tear_block"
    fi
else
    bad "acceptance.yml exists" "not found at $ACC_YML"
fi

# Positive control: a fixture teardown block missing pve-ca-cert is detected.
# Strip pve-ca-cert from the real teardown block and verify the check flags it.
if [ -n "$_gate_tear_block" ] && [ -n "$_tear_required" ]; then
    _fixture_no_cert="$(printf '%s\n' "$_gate_tear_block" | grep -v 'pve-ca-cert')"
    _detected=""
    while IFS= read -r _inp; do
        [ -n "$_inp" ] || continue
        case "$_fixture_no_cert" in
            *"$_inp"*) ;;
            *) _detected="$_detected $_inp" ;;
        esac
    done <<< "$_tear_required"
    case "$_detected" in
        *pve-ca-cert*) ok "positive control: fixture missing pve-ca-cert is detected" ;;
        *) bad "positive control: fixture missing pve-ca-cert is detected" \
               "required-input check did not flag pve-ca-cert as missing from the fixture" ;;
    esac
fi

echo
echo "18. acceptance fires via App-token tag push, not manual dispatch:"
# GitHub's anti-recursion rule: a tag pushed with GITHUB_TOKEN triggers no
# workflow. The cut job mints an App installation token and pushes the release
# tag with it; an App-pushed tag is not subject to the rule, so acceptance.yml's
# push trigger fires directly. Positive control: a cut block that does not exist
# leaves every want below reporting missing content.
_cut_block="$(awk '/^  cut:$/{f=1;next} f&&/^  [a-z_-]+:$/{exit} f{print}' "$GATE_YML")"
if [ -z "$_cut_block" ]; then
    bad "cut job block located (positive control)" "awk extracted nothing; assertions below would be vacuous"
else
    ok "cut job block located (positive control)"
fi
want "cut mints App token"           "app-tok"                  "$_cut_block"
want "cut reads App private key"     "SPIRA_GH_APP_PRIVATE_KEY" "$_cut_block"
want "cut tag push uses App token"   "app-tok.outputs.token"    "$_cut_block"
# Minting the token into GH_TOKEN is not enough: git ignores GH_TOKEN and
# authenticates with whatever credential actions/checkout persisted (GITHUB_TOKEN),
# so the token must be handed to git explicitly. Positive control first: a bare
# push (the pre-fix shape) must fail the checks below, or they are vacuous.
_bare_push_fixture='git push origin "refs/tags/$tag"'
nowant "positive control: bare push lacks a credential helper" \
       "credential.helper" "$_bare_push_fixture"
want "cut tag push clears the checkout-persisted credential" \
     "extraheader" "$_cut_block"
want "cut tag push routes through a credential helper" \
     "credential.helper" "$_cut_block"
# The cut job must output prev-tag so acceptance.yml can derive the upgrade path.
_cut_out_block="$(echo "$_cut_block" \
  | awk '/^    outputs:/{g=1;next} g&&/^    [a-z_-]+:/{exit} g{print}')"
want "cut outputs prev-tag"                    "prev-tag"        "$_cut_out_block"
# acceptance.yml must still accept workflow_dispatch with tag and prev-tag inputs.
if [ -r "$ACC_YML" ]; then
    A="$(cat "$ACC_YML")"
    want "positive control: acceptance.yml was read"  "workflow_dispatch" "$A"
    want "acceptance.yml accepts tag input"           "tag:"              "$A"
    want "acceptance.yml accepts prev-tag input"      "prev-tag:"         "$A"
    # The comment must no longer claim push fires automatically for every release.
    nowant "comment no longer claims push auto-tests every release" \
           "automatically tests each release" "$A"
else
    bad "acceptance.yml exists for cross-check" "not found at $ACC_YML"
fi

echo
echo "19. provision is skipped when no suites are selected:"
# A branch touching only statutes or docs selects zero suites. Acquiring a 12 GiB
# machine to run nothing wastes a runner slot and starves a real gate. provision
# is conditional on the select job outputting a non-empty list; gate exits 0
# (green, not 75) when the list is empty.
_prov_block="$(awk '/^  provision:$/{f=1;next} f&&/^  [a-z_-]+:$/{exit} f{print}' "$GATE_YML")"
if [ -z "$_prov_block" ]; then
    bad "the provision job block was located (positive control)" "awk extracted nothing"
else
    ok "the provision job block was located (positive control)"
fi
want "provision needs the select job"                "select"                      "$_prov_block"
want "provision is conditional on suite selection"   "needs.select.outputs.suites" "$_prov_block"
# The gate job exits 0 on an empty selection so branch protection sees green, not
# skipped or failed — a skipped required check blocks the merge.
want "gate exits cleanly on empty selection"         "exit 0"                      "$_gate_verdict_block"
want "gate checks select output before provision"    "select.outputs.suites"       "$_gate_verdict_block"

echo
echo "20. the provision job raises the guest-agent wait above the 120s default:"
# provision.sh's guest-agent wait defaults to 120s, and that sits inside the
# normal boot range on a busy hypervisor -- a fault, not a defect in the guest.
want "provision sets AGENT_TIMEOUT"        "AGENT_TIMEOUT"       "$_prov_block"
want "provision raises it to 300"          "AGENT_TIMEOUT: 300"  "$_prov_block"

echo
printf '  %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
