#!/usr/bin/env bash
#
# gate-spira.sh — this repository's scheduled test runner.
#
#   gate-spira.sh
#
# THE PRODUCTION LANDING GATE IS NOT THIS SCRIPT. The landing gate for this repository is
# the CMD in the repo-map (the fence scripts run directly). This script is invoked by
# suites.sh on a timer; when it is red it files a bead, but it does not block landings.
# A branch lands when the repo-map CMD passes, not when this script passes. Being red here
# is a fact for the timed run and for whoever can fix the failing suite — not for the gate.
# Decision: sp-wyep. Scar: gate-spira.sh was red at 16:33 UTC 2026-09-12 while nineteen
# commits landed unimpeded; the mechanism is that the landing gate is the fence CMD, not
# this script (sp-50l).
#
# WHAT THIS REPLACED, and why. Until 2026-09-07 this ran 43 suites, 13,098 lines of test
# code against 4,642 lines of program, and took 17 minutes per branch. On the day the system
# spent an entire morning unable to land anything, that gate found zero real defects and
# produced two failures — both of them suites reading the state of the box they ran on rather
# than the code under test. It was also, by being 17 minutes long, most of the contention
# that made a shared worktree worth locking, and the lock is what livelocked the queue.
#
# The three real defects that day were found by two things: tests written for the specific
# change, in minutes, and a 20-second soak. Neither was in the 17 minutes.
#
# WHAT THIS SYSTEM ACTUALLY IS: N workers pull from a DAG, put candidate work in a merge
# queue, and the queue is CI/CD. Every failure it has ever had is a property of that pipeline
# — two things running at once, a lock, a queue that stops moving — and none has been a
# function returning the wrong value. So the gate checks the pipeline.
#
#   1. THE FENCES. Not quality checks: guards against things that cannot be undone by
#      deleting a commit. A published beads database holds internal notes, agent memories and
#      the operator's own judgement; a published operator inventory names one person's
#      machine. These run first, independently, and nothing routes around them.
#      (hermetic.sh and the fixture-contamination fence were here until sp-1ctx: both detected
#      suites reaching the running system, damage that running every suite in a container
#      prevents. host-check.sh carries the host-escape declaration requirement — see it and
#      suites.sh status for the migration count.)
#   2. THE SOAK. Does the merge queue make progress while several gates and a landing pass
#      run against each other? It reproduces the 2026-09-07 livelock against the old code at
#      the production ratio, then shows it gone.
#   3. THE END-TO-END is filed, not built (sp-canary). One bead through the whole pipeline
#      on a staging Spira, with a commit on the base branch at the end of it, is the only
#      check that would test the claim this system makes about itself — and it needs a whole
#      second Spira stood up, which is not what today is for. Detection comes first
#      (law-detection-outranks-rejection): the watchtower notices the pipeline has stopped
#      and files it back into the DAG, which is worth more than a gate refusing a change.
#
# It fails CLOSED, and a check that could not run is not a pass.
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
# IT JUDGES THE TREE IT IS PART OF, and it finds that tree from its own path. This read
# `cd "${SPIRA_GATE_REPO:-.}"`, and the gate sets SPIRA_GATE_REPO to the INSTALLED CHECKOUT
# while extracting the branch to a scratch worktree and running the command there — so this
# line stepped out of the tree under trial and ran the installed copy's suites instead. Every
# branch was therefore judged against the code already in force, which is the one failure a
# gate must not have: it passes work it never looked at, and the landing pass acts on the
# pass. A branch that only edits existing files sails through on the installed copy's green.
# It surfaced only when a branch ADDED a suite, which the installed copy did not have.
#
# Self-located rather than configured, because where the harness IS is a fact about where
# this script sits and never a setting (law-a-split-repoints-nothing). The fence readability
# checks below are the identity check on the result, and they fail closed.
cd "$HERE/.." 2>/dev/null || { printf 'gate: cannot reach the tree holding %s\n' "$0" >&2; exit 1; }

rc=0
gate_total=0      # wall-clock seconds summed across all suites
gate_unmeasurable=0  # set to 1 if any suite's cost cannot be measured
say() { printf 'gate: %s\n' "$*" >&2; }

# file_budget_bead <total> <budget> <over> — file a bead when the gate exceeds its budget.
# Runs in a subshell so sourcing lib.sh does not pollute this script's stripped environment.
# Failure to file must not change the gate's verdict; the caller passes `|| true`.
# SPIRA_DB from the environment wins by conf.sh's env-first rule, so a test database set
# before this script was invoked is the one that receives the bead — not the operator's live
# store. In production, HOME is set (by gate.sh's env -i) so conf.sh finds spira.conf.
# THE BEAD CARRIES repo:spira. The test harness must supply a repo-map that lists "spira"
# so _bdq_check_repo_label allows the create: test-gate-budget.sh creates $SH/repo-map for
# this. Without it the check refuses quietly (no bead filed, gate still exits cleanly). (sp-2kpk2)
file_budget_bead() {
    local total="$1" budget="$2" over="$3"
    (
        . "$HERE/lib.sh" 2>/dev/null || exit 0
        local ref="gate:budget"
        # Dedupe: only file if no open bead with this ref already exists.
        # --external-ref is a create flag, not a bd list flag — filter by the field in JSON.
        local existing
        existing="$(bdq list --status open --limit 0 --json 2>/dev/null \
            | python3 -c 'import sys,json
d=json.load(sys.stdin)
match=[x for x in (d if isinstance(d,list) else []) if x.get("external_ref")=="gate:budget"]
print(match[0]["id"] if match else "")
' 2>/dev/null || true)"
        [ -n "${existing:-}" ] && exit 0
        bdq create \
            "gate: budget exceeded — ${over}s over (${total}s vs ${budget}s); something must leave gate-suites before anything joins" \
            --type chore --priority 2 \
            --labels "${SPIRA_SCOPE_LABEL:+$SPIRA_SCOPE_LABEL,}${SPIRA_PLAN_LABEL},repo:spira" \
            --external-ref "$ref" \
            --body - --silent >/dev/null 2>&1 <<'BODY'
The gate has run longer than SPIRA_GATE_BUDGET. Before adding any suite to
spira/gate-suites, one must leave: the budget is what forces that argument, and without
it each suite was individually justified while the total reached 17 minutes unchecked.
BODY
    ) 2>/dev/null || true
}

# ---------------------------------------------------------------------------------------
# 1. THE FENCES — first, and independently of everything below.
# ---------------------------------------------------------------------------------------
for fence in spira/exclude.sh spira/inventory.sh spira/scratch-fence.sh spira/wiki-add-fence.sh spira/literal-lint.sh spira/suite-state-fence.sh spira/orphan-test.sh; do
    [ -r "$fence" ] || { say "$fence is missing — refusing to land unchecked"; exit 1; }
done

# The whole tree, not the diff: the question is what this repository would CONTAIN after
# landing, so a database that arrived on an earlier commit of the same branch is caught
# rather than waved through for not being in this diff.
offenders="$(git ls-files 2>/dev/null | bash spira/exclude.sh filter 2>/dev/null)"
if [ -n "$offenders" ]; then
    say "this branch would land beads data in the harness tree:"
    printf '%s\n' "$offenders" | sed 's/^/gate:   /' >&2
    say "a beads database is never public. There is no override for this one."
    exit 1
fi

if ! inv="$(bash spira/inventory.sh 2>&1)"; then
    printf '%s\n' "$inv" >&2
    exit 1
fi

# SCRATCH-FILE FENCE. Aeon working notes committed to the harness root ship to every
# consumer and trigger the all-suites fallback in coverage selection (a file no suite
# declares widens the diff to the full corpus, ~65 min). The class of defect is repeatable:
# ten accumulated before this fence existed. SCRATCH_FENCE_OK=1 is the named override,
# valid only for the commit that removes existing offenders.
[ -r spira/scratch-fence.sh ] || { say "spira/scratch-fence.sh is missing — refusing to land unchecked"; exit 1; }
if ! scr="$(bash spira/scratch-fence.sh 2>&1)"; then
    printf '%s\n' "$scr" >&2
    exit 1
fi
printf '%s\n' "$scr" >&2

# WIKI BLANKET-ADD FENCE. Blanket staging on the wiki checkout (git add -A, git add .,
# git commit -a) sweeps another actor's uncommitted work into the commit and manufactures
# false attribution (incident: sp-4fl2e). wiki-commit.sh is the canonical path and stages
# files explicitly; this fence keeps that pattern in force.
[ -r spira/wiki-add-fence.sh ] || { say "spira/wiki-add-fence.sh is missing — refusing to land unchecked"; exit 1; }
if ! waf="$(bash spira/wiki-add-fence.sh 2>&1)"; then
    printf '%s\n' "$waf" >&2
    exit 1
fi
printf '%s\n' "$waf" >&2

# THE SOP SHELF. `sop.sh write` validates; `bd remember sop-<slug>` does not — it is the
# back door this check closes. Lint reads every sop- key and applies the same rules, so a
# runbook written directly cannot survive to be matched at 3am. An unreadable shelf fails
# closed: a broken database is not evidence that no malformed SOPs exist
# (law-absence-needs-a-positive-control, law-bake-rules-into-tools).
[ -r spira/sop.sh ] || { say "spira/sop.sh is missing — refusing to land unchecked"; exit 1; }
if ! sop_lint="$(bash spira/sop.sh lint 2>&1)"; then
    printf '%s\n' "$sop_lint" >&2
    say "sop lint FAILED — the shelf has malformed entries; fix with sop.sh write or sop.sh retire"
    exit 1
fi

# CONFIGURED-NAME LITERAL FENCE. schema.sh is the one place that declares label, status
# and type names; a name written as a literal in any other source file can disagree with
# the declaration when an operator changes the default. lib.sh:117 grepped "needs-ryan"
# while lib.sh:221 read ${SPIRA_ASK_LABEL:-needs-ryan}, and the code default is
# needs-operator — so the destructive-procedure fence had no bypass and every halting bead
# was refused on a default install. One accessor per name makes that class unwritable.
[ -r spira/literal-lint.sh ] || { say "spira/literal-lint.sh is missing — refusing to land unchecked"; exit 1; }
if ! lit="$(bash spira/literal-lint.sh 2>&1)"; then
    printf '%s\n' "$lit" >&2
    exit 1
fi

# SUITE-STATE FENCE. A quarantine entry that names a CLOSED bead has no exit path —
# suites.sh hygiene requires land_state:LANDED, which CLOSED never satisfies. The check
# also applies suite_state_lint: unknown state, missing reason, bead-less quarantine,
# named suite that does not exist in the tree.
[ -r spira/suite-state-fence.sh ] || { say "spira/suite-state-fence.sh is missing — refusing to land unchecked"; exit 1; }
if ! ssf="$(bash spira/suite-state-fence.sh 2>&1)"; then
    printf '%s\n' "$ssf" >&2
    exit 1
fi
printf '%s\n' "$ssf" >&2

# ORPHAN-TEST FENCE. A diff that removes a hyphenated token from a non-test source file
# while a test suite that is NOT in the same diff still asserts on that token orphans that
# test: it goes red on the next timed run, pointed at the correct source file, and the beads
# are filed against code that is correct. This is rung 4 (a program that refuses) after two
# independent re-violations of law-prefer-the-real-dependency.
#
# It exits 77 (skip) when SPIRA_GATE_BASE is absent — the scheduled test runner has no diff
# to analyze. That skip is recorded and is not a pass. When running from the landing gate
# (SPIRA_GATE_BASE is in the environment), an exit 77 is still logged and not treated as a
# pass by the gate's run() function above.
[ -r spira/orphan-test.sh ] || { say "spira/orphan-test.sh is missing — refusing to land unchecked"; exit 1; }
ot_rc=0
ot="$(bash spira/orphan-test.sh 2>&1)" || ot_rc=$?
if [ "$ot_rc" -ne 0 ] && [ "$ot_rc" -ne 77 ]; then
    printf '%s\n' "$ot" >&2
    exit 1
fi
# Exit 77 means the fence skipped (no diff context) — reported but not a branch failure.
[ "$ot_rc" -eq 77 ] && say "orphan-test SKIPPED — $(printf '%s' "$ot" | head -1)"
# Print the clean line on success so gate output is consistent with the other fences.
[ "$ot_rc" -eq 0 ] && printf '%s\n' "$ot" >&2 || true

# ---------------------------------------------------------------------------------------
# 2 AND 3 — the pipeline. Both are real programs run against each other; neither models
# anything (law-prefer-the-real-dependency).
#
# A SKIP IS ANNOUNCED, NEVER SWALLOWED. Both exit 77 — the automake convention — when the
# box cannot host them: no flock, or no fixture database server. A gate that reported "could
# not check" as all-clear would displace the suspicion that would have prompted a look
# (law-alerts-must-be-actionable).
# ---------------------------------------------------------------------------------------

# INLINE MODE. When SPIRA_IN_TESTENV=1 (already inside a container — calling testenv-batch
# again would nest containers), or when SPIRA_GATE_INLINE=1 (test context: the scratch tree
# has no container support), run suites directly on the host.
# Containment is not waived in either case: the caller guarantees an isolated context.
INLINE=0
[ -n "${SPIRA_IN_TESTENV:-}" ] && INLINE=1
[ -n "${SPIRA_GATE_INLINE:-}" ] && INLINE=1

run() {                  # run <suite> — its output only when it matters; cost always
    local s="$1" out st t0 t1 elapsed elapsed_s name suite_pid killer tmp
    name="$(basename "$s")"
    t0=$(date +%s 2>/dev/null) || t0=""
    tmp="$(mktemp)" || { say "could not create temp file for $name"; rc=1; return; }
    # PROCESS GROUP ISOLATION. The original command substitution creates a pipe; a background
    # job inheriting that pipe's write end blocks the gate until it exits or is killed. Running
    # via setsid (suite gets PGID = suite_pid) lets us sweep survivors with kill -- -$suite_pid
    # after the suite exits, so no orphan can hold any descriptor open (sp-a8c5).
    setsid bash "$s" > "$tmp" 2>&1 &
    suite_pid=$!
    # KILLER IN ITS OWN PROCESS GROUP so that `kill -- -$killer` sweeps both the sh and the
    # `sleep` child in one shot.  Without setsid, `sleep` orphans in the test script's PGID
    # and the harness detects it as a background job left after exit (sp-u5y4t).
    setsid bash -c "sleep ${SPIRA_SUITE_TIMEOUT:-600} && kill -- -${suite_pid} 2>/dev/null" &
    killer=$!
    wait "$suite_pid" 2>/dev/null; st=$?
    local _had_survivor=0
    kill -0 -- -"$suite_pid" 2>/dev/null && _had_survivor=1 || true
    kill -- -"$killer" 2>/dev/null; wait "$killer" 2>/dev/null || true
    [ "$st" -ge 128 ] && st=124
    if [ "$_had_survivor" = 1 ]; then
        sleep 0.2
        if kill -0 -- -"$suite_pid" 2>/dev/null; then
            local _orphan=0 _op
            for _op in $(pgrep -g "$suite_pid" 2>/dev/null); do
                [ "$(awk '{print $4}' /proc/"$_op"/stat 2>/dev/null)" = "1" ] \
                    && { _orphan=1; break; }
            done
            if [ "$_orphan" = 1 ]; then
                printf '\nFAIL: %s left background jobs after exit — killed by gate harness\n' "$name" >> "$tmp"
                kill -- -"$suite_pid" 2>/dev/null || true
                [ "$st" -eq 0 ] && st=1
            fi
        fi
    fi
    out="$(cat "$tmp")"; rm -f "$tmp"
    t1=$(date +%s 2>/dev/null) || t1=""
    # COST IS MEASURED AROUND THE SUBPROCESS, never inferred. If date fails on either side
    # the cost is unmeasurable; unmeasurable counts as over-budget rather than free
    # (law-absence-needs-a-positive-control). The ? is rendered in the log so the reader
    # knows the measurement failed rather than seeing a suspiciously small number.
    if [ -n "$t0" ] && [ -n "$t1" ]; then
        elapsed=$(( t1 - t0 ))
        elapsed_s="${elapsed}s"
        gate_total=$(( gate_total + elapsed ))
    else
        elapsed_s="?"
        gate_unmeasurable=1
    fi
    case "$st" in
        0)  printf 'gate: %-22s ok   cost=%s\n' "$name" "$elapsed_s" >&2 ;;
        77) printf 'gate: %-22s SKIPPED — cost=%s — %s\n' "$name" "$elapsed_s" \
                "$(printf '%s' "$out" | sed -n 's/.*SKIP *//p' | head -1)" >&2 ;;
        124) printf '%s\n' "$out" >&2
             say "$name was killed at ${SPIRA_SUITE_TIMEOUT:-600}s (cost=$elapsed_s)"; rc=1 ;;
        *)  printf '%s\n' "$out" >&2
            say "$name FAILED (rc=$st, cost=$elapsed_s)"; rc=1 ;;
    esac
}

# WHICH SUITES, AND WHY EACH — in `spira/gate-suites`, one path per line with its reason
# beside it. It is a file rather than a list on the line below for one reason: nothing else
# could read the list while it lived here, so no program could answer "which suites does the
# gate NOT run", and the answer went unexamined until five of nine suites were running
# nowhere at all. `suites.sh` runs the complement on a timer, derived from the same file, so
# a suite dropped from the gate moves to the timed run rather than out of the world.
SUITE_LIST=spira/gate-suites
[ -r "$SUITE_LIST" ] || { say "$SUITE_LIST is missing — refusing to report a pass without it"; exit 1; }
# Read whole first, then run: a list that names a file which does not exist is a gate
# reporting on work it never looked at, and finding that out halfway through means the
# suites before the bad line have already been paid for.
suites=""
while IFS= read -r s || [ -n "$s" ]; do
    s="${s%%#*}"; s="${s#"${s%%[![:space:]]*}"}"; s="${s%"${s##*[![:space:]]}"}"
    [ -n "$s" ] || continue
    [ -r "$s" ] || { say "$SUITE_LIST names $s, which is not here — refusing to report a pass without it"; exit 1; }
    suites="$suites $s"
done < "$SUITE_LIST"
# AN EMPTY LIST IS A FAILURE, NOT AN EMPTY GATE (law-absence-needs-a-positive-control). A
# truncated or all-comment file would otherwise run nothing and exit 0, which is the one
# verdict a gate must never reach by accident.
[ -n "$suites" ] || { say "$SUITE_LIST names no suite — refusing to report a pass on nothing"; exit 1; }

# COVERAGE-BASED SELECTION OF NON-GATED SUITES via select.sh.
# When SPIRA_GATE_FILES names a readable file (set by gate.sh when calling a
# repository's own gate command), select.sh is asked for the diff-derived suite
# set. Results that duplicate a gated suite are skipped; the gated list always
# runs regardless of diff.
#
# When SPIRA_GATE_FILES is not set — a direct invocation, or an inner call from
# a test fixture such as test-gate-budget.sh — only the gated list runs. This
# is deliberate: an inner invocation that inherited the full non-gated set would
# re-enter test-gate-budget.sh itself and exceed the 602s per-suite watchdog on
# every timed pass.
extra_suites=""
if [ -f "${SPIRA_GATE_FILES:-}" ]; then
    # select.sh owns the selection algorithm; this script owns which suites are
    # already in the gated list. Deduplicate so each suite runs at most once.
    # --files passes the pre-computed list from gate.sh directly rather than
    # re-computing the diff — avoids double work and lets test fixtures supply
    # the list without needing git refs.
    # Run select.sh synchronously in a command substitution so it fully exits before
    # we read the mode file. A process substitution (< <(bash select.sh ...)) leaves
    # select.sh running as a background subshell that can block in a container
    # environment when a suite exits non-zero — the background process holds open
    # file descriptors that delay gate-spira.sh's cleanup path.
    _cv_mode_file="$(mktemp)"
    _cv_raw="$(bash "$HERE/select.sh" \
        --files "${SPIRA_GATE_FILES}" \
        --mode-file "$_cv_mode_file" \
        2>/dev/null || true)"
    _cv_mode="$(cat "$_cv_mode_file" 2>/dev/null || true)"; rm -f "$_cv_mode_file"
    while IFS= read -r _s || [ -n "$_s" ]; do
        [ -n "$_s" ] || continue
        _cv_path="spira/$_s"
        # Skip any suite already in the gated list.
        case " $suites " in
            *" $_cv_path "*) ;;
            *) extra_suites="$extra_suites $_cv_path" ;;
        esac
    done <<< "$_cv_raw"
    _cv_n=0; for _cv_ts in $extra_suites; do _cv_n=$((_cv_n + 1)); done
    if [ "$_cv_mode" = all ]; then
        say "coverage: unmapped file(s) in diff — running all non-gated suites"
    elif [ "$_cv_n" -gt 0 ]; then
        say "coverage: selected $_cv_n non-gated suite(s)"
    fi
fi

if [ "$INLINE" = 1 ]; then
    for s in $suites $extra_suites; do
        run "$s"
    done
else
    # Route through testenv-batch.sh: one container, suites as basenames on stdin.
    _tb_t0=$(date +%s 2>/dev/null) || _tb_t0=""
    {
        for s in $suites $extra_suites; do
            printf '%s\n' "$(basename "$s")"
        done
    } | bash "$HERE/testenv-batch.sh" --suites - "${SPIRA_GATE_SELECT_HEAD:-HEAD}" >&2
    _tb_rc=${PIPESTATUS[1]}
    _tb_t1=$(date +%s 2>/dev/null) || _tb_t1=""
    if [ -n "$_tb_t0" ] && [ -n "$_tb_t1" ]; then
        gate_total=$(( _tb_t1 - _tb_t0 ))
    else
        gate_unmeasurable=1
    fi
    case "$_tb_rc" in
        0) ;;
        1) rc=1 ;;
        2) say "testenv-batch: container failed to start — suites did not run (harness fault)"; rc=1 ;;
        3) say "testenv-batch: install failed in container — suites did not run (harness fault)"; rc=1 ;;
        *) say "testenv-batch exited $_tb_rc — suites did not run"; rc=1 ;;
    esac
fi

# ---------------------------------------------------------------------------------------
# BUDGET CHECK. The gate times itself and reports what it cost, on every run, so the timed
# run and the cockpit pane can both read it. Exceeding the budget is NOT a branch failure —
# the branch did not cause the overrun. It is a failure of the gate, reported against the
# harness: something must leave spira/gate-suites before anything else joins.
#
# AN UNMEASURABLE COST IS OVER BUDGET, NEVER FREE. A suite whose wall-clock cannot be
# read renders `?` and the gate treats it as an overrun, because a measurement that reports
# zero for the wrong reason is worse than one that flags uncertainty.
# ---------------------------------------------------------------------------------------
gate_budget="${SPIRA_GATE_BUDGET:-300}"
if [ "$gate_unmeasurable" -eq 1 ]; then
    say "cost total=?s budget=${gate_budget}s (one or more suite costs were unmeasurable)"
    file_budget_bead "?" "$gate_budget" "?" || true
elif [ "$gate_total" -gt "$gate_budget" ]; then
    over=$(( gate_total - gate_budget ))
    say "cost total=${gate_total}s budget=${gate_budget}s EXCEEDED by ${over}s"
    say "this is a gate fault, not a branch fault — filing a bead against the harness"
    say "something must leave spira/gate-suites before anything else joins it"
    file_budget_bead "$gate_total" "$gate_budget" "$over" || true
else
    say "cost total=${gate_total}s budget=${gate_budget}s"
fi

exit "$rc"
