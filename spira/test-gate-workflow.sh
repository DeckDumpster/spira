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
# covers: .github/workflows/gate.yml .github/workflows/release.yml
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
ROOT="$(cd "$HERE/.." && pwd -P)"
pass=0; fail=0
ok()   { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()  { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "${2:-}"; }
want() { case "$3" in *"$2"*) ok "$1" ;; *) bad "$1" "wanted [$2]" ;; esac; }

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
echo "2. a push to the base branch names the corpus explicitly:"
# --suites is how a caller names what to run; without it the base-branch diff is
# empty and the gate would pass having run nothing.
want "the corpus is named with --suites" "--suites" "$G"

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
printf '  %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
