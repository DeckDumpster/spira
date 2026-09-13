#!/usr/bin/env bash
#
# gate.sh — the deterministic landing gate. Exits 0 if a branch may merge.
#
#   gate.sh <branch> [repo-name]
#
# This is the Cloister: a trial a branch passes before it may enter. It is deliberately
# cheap and mechanical — syntax, then whatever the repository itself declares — because a
# gate that needs judgement is not a gate, it is a review. Judgement belongs to sentinel.sh
# CHECK 8 and to the operator.
#
# TWO LAYERS, AND ONLY ONE OF THEM IS UNIVERSAL. `bash -n` on every changed shell script
# holds everywhere, because a script that does not parse is the most common way an
# unattended change breaks a harness. Everything else is the REPOSITORY'S OWN: brain runs
# the harness and guard suites, another runs a Rust formatting check with its own CI
# behind the pull request. That command comes from repo-map, so adding a repository is
# adding a line rather than editing this file — which is what "the landing gate must learn
# each repo's own test command" means in practice.
#
# It fails CLOSED. A gate that cannot run its own checks must not report a pass: the whole
# point is to keep unverified work out of a branch everything else pulls from.
#
# FAILING CLOSED IS NOT THE SAME AS BLAMING THE BRANCH, and conflating them is what this gate
# spent two days doing. Every exit here is one of four outcomes (conf.sh): PASS, FAIL,
# BASE_FAIL, NO_VERDICT. The verdict is withheld identically in the last two — nothing lands
# — but only FAIL says the branch is at fault, and only FAIL may cost it an attempt. Before
# this, a missing worktree, a lock, a deadline and a repository whose suites fail on its own
# base all arrived at the landing pass as "this branch is broken", and three of those poison
# a bead and page the operator about work that was fine.
set -uo pipefail
. "$(dirname "$0")/lib.sh"

# verdict <status> <reason-slug> [message ...] — the ONLY way out of this program.
#
# One exit point, because the classification is the whole contract and a bare `exit 1`
# somewhere in the preflight is how the contract gets broken silently. It meters, it says the
# outcome in a form both a human and `landing.sh` can read, and it enforces the one
# invariant that cannot be left to the caller: a FAIL must be able to show its work.
#
# A FAIL THAT SAYS NOTHING AT ALL IS DOWNGRADED TO NO_VERDICT, here, as a backstop. The
# base..branch diff failure exited 1 without a word (sp-io5j), and this makes such an exit
# structurally impossible rather than relying on every future call site to remember. The
# richer form of the same rule — a gate command that ran and printed nothing — is enforced
# where `$out` is in scope, because by the time it reaches here it is wrapped in prose.
verdict() {              # verdict <status> <reason> [message...]
    local st="$1" reason="$2"; shift 2
    local msg="$*"
    if [ "$st" != 0 ] && [ "$st" != "$SPIRA_GATE_NOVERDICT" ] && [ "$st" != "$SPIRA_GATE_BASEFAIL" ] \
       && [ -z "${msg//[[:space:]]/}" ]; then
        reason="no-evidence:$reason"; st="$SPIRA_GATE_NOVERDICT"
        msg="the gate returned a failure with no output at all — that is the machinery failing to run a check, not the branch failing one"
    fi
    [ -n "$msg" ] && printf '%s\n' "$msg" >&2
    # The machine-readable line. Anchored and single, so a caller matches the whole shape
    # rather than grepping prose that a reword would silently change.
    #
    # IT CARRIES THE SUITE BECAUSE THE CALLER HAS TO KEY ON SOMETHING. A BASE_FAIL is filed
    # by the landing pass as one incident against the repository, deduped on an external ref,
    # and a ref built from prose is a ref that changes the day somebody rewords a message —
    # at which point one broken base files a fresh bead every pass instead of bumping one.
    # `-` when the repository's gate named nothing identifiable, which is a stable key too.
    printf 'gate: VERDICT=%s reason=%s branch=%s repo=%s suite=%s\n' \
        "$(spira_gate_outcome "$st")" "$reason" "$BR" "${REPO_NAME:-?}" "${GATE_SUITE:--}" >&2
    # THE EXIT TRAP IS DISARMED FIRST. It exists to meter the ways out that do not come
    # through here — a `set -e` death, a signal — and if it survived this call every verdict
    # would be metered twice, the second time with the status of whatever ran last inside the
    # trap rather than the verdict's own. Cleanup that the trap owns is done here instead.
    trap - EXIT
    rm -f "${FILELIST:-}" 2>/dev/null
    # REMOVE THE GATE WORKTREE ON EVERY EXIT PATH — success, failure, timeout, kill — so the
    # registration does not outlive the run. TREE may be unset when verdict() is called before
    # the tree was created (early preflight failures); guard before testing for it. The flock
    # is still held at this point, so no concurrent gate can be using the tree.
    if [ -n "${TREE:-}" ] && [ -e "${TREE}/.git" ]; then
        git -C "${REPO:-/nonexistent}" worktree remove --force "$TREE" 2>/dev/null || true
    fi
    # gate_meter is defined only once the tree lock has been reached; before that there is no
    # wait and no run to record, and a preflight refusal is not a reading about contention.
    command -v gate_meter >/dev/null 2>&1 && gate_meter "$st" "$reason"
    # AND IT RECORDS WHAT THE GATE WAS WORTH, not only what it cost. Same placement and same
    # reasoning as the meter: this is the one way out, so a red that is never recorded is a
    # red that never happened as far as anybody measuring this gate is concerned. It can fail
    # and is not allowed to matter — a gate whose verdict changed because its bookkeeping
    # broke would be worse than not measuring at all.
    command -v yield_note >/dev/null 2>&1 && yield_note "$st" "$reason"
    exit "$st"
}
NV="$SPIRA_GATE_NOVERDICT"

BR="${1:?usage: gate.sh <branch> [repo-name]}"
REPO_NAME="${2:-$(spira_home_repo)}"

# AN UNREADABLE REPOSITORY MAP IS A MACHINERY FAULT, and it used to be a PASS. With no map,
# `repo_gate` returns an empty command, an empty command means "syntax was the whole trial",
# and the gate exited 0 — so a mistyped path, an unmounted home or a map deleted by a bad
# install silently turned every repository into one with no gate at all, and every branch
# passed a trial that never happened. Distinguish it from the legitimate case: a map that IS
# readable, naming a repository with an empty gate column, is a repository whose trial really
# is syntax alone.
[ -r "${SPIRA_REPO_MAP:-/nonexistent}" ] || verdict "$SPIRA_GATE_NOVERDICT" no-repo-map-file \
    "gate: the repository map at ${SPIRA_REPO_MAP:-<unset>} cannot be read — refusing to judge.
gate: without it every repository looks like one with no gate command, and every branch
gate: would pass a trial that never ran."
REPO="$(repo_root "$REPO_NAME")" || verdict "$NV" no-repo-map \
    "gate: repo-map has no entry for '$REPO_NAME' — refusing to guess a checkout"

# ---------------------------------------------------------------------------------------
# WHAT THIS GATE IS WORTH. The meter below records what a run COST; this records whether the
# run was right. A gate may sit between work and its landings only while it is catching real
# defects (law-gate-earns-its-place), and the previous one was deleted after twelve hours of
# fallout rather than on the evidence, because the evidence did not exist. yield.sh carries
# the whole argument; what belongs here is the three facts only the gate holds.
#
# THE TREE, because it is what distinguishes a branch that was fixed from a branch that was
# refused and then passed unchanged — the first is a defect the gate caught, the second is
# the gate contradicting itself, and nothing else can tell them apart afterwards.
GATE_TREE="$(git -C "$REPO" rev-parse --verify -q "$BR^{tree}" 2>/dev/null)" || GATE_TREE=""
[ -n "$GATE_TREE" ] || GATE_TREE=-
# THE SUITE, filled in below from the gate command's own output when it names one. `-` until
# then, and `-` forever if the repository's gate says nothing identifiable — a suite named on
# a guess is worse than a red attributed to the gate's reason, because the wrong suite is the
# one somebody deletes.
GATE_SUITE=-
YIELD="$(dirname "$0")/yield.sh"
yield_note() {           # yield_note <status> <reason>
    [ -r "$YIELD" ] || return 0
    # SPIRA_RUN IS PASSED EXPLICITLY. conf.sh does not export it, so a child re-deriving it
    # from a config file would write its record somewhere this process is not reading — which
    # is the silent half of a measurement that reports zero for the wrong reason.
    if [ "$1" = 0 ]; then
        SPIRA_RUN="$SPIRA_RUN" bash "$YIELD" pass "$REPO_NAME" "$BR" "$GATE_TREE" >/dev/null 2>&1
    else
        SPIRA_RUN="$SPIRA_RUN" bash "$YIELD" record \
            "$REPO_NAME" "$BR" "$1" "$2" "$GATE_TREE" "$GATE_SUITE" >/dev/null 2>&1
    fi
    return 0
}

# The remote-tracking ref, never the local branch. Nothing in this harness advances the
# shared checkout's default branch, so a diff against it is a diff against whatever the last
# human left behind: the changed-file list would include every file that landed since, and
# the gate would then run — or skip — suites on the strength of somebody else's work. The
# branch reaching here has already been rebased onto this same ref by CHECK 6, so the
# three-dot merge base is its real base.
#
# And it is resolved, not assumed to be `main`: some repositories have no such ref, so
# every diff here was against nothing and every changed-file test read empty — a gate that
# skips every suite because it thinks nothing changed passes everything.
BASE="$(spira_landref "$REPO")" || verdict "$NV" no-base \
    "gate: cannot resolve the ref '$REPO_NAME' lands on — refusing to guess a base
gate: give it a \`base\` column in $SPIRA_REPO_MAP"
# THE ONE REFUSAL THAT USED TO SAY NOTHING (sp-io5j). A failed diff means the branch or the
# base does not resolve in this checkout — a fact about the checkout, never about the work.
_diff_out="$(git -C "$REPO" diff --name-only "$BASE...$BR" 2>&1)" || verdict "$NV" no-diff \
    "gate: cannot diff $BASE...$BR in $REPO_NAME — one of them does not resolve in this checkout
$_diff_out"
files="$_diff_out"

# ---------------------------------------------------------------------------------------
# LAYER 1 — universal. Every changed shell script must parse.
# ---------------------------------------------------------------------------------------
while IFS= read -r f; do
    [ -n "$f" ] || continue
    case "$f" in
        *.sh)
            git -C "$REPO" show "$BR:$f" > /tmp/spira-gate-$$.sh 2>/dev/null || continue
            if ! syntax="$(bash -n /tmp/spira-gate-$$.sh 2>&1)"; then
                rm -f /tmp/spira-gate-$$.sh
                verdict 1 syntax "gate: $f fails bash -n
$syntax"
            fi
            rm -f /tmp/spira-gate-$$.sh
            ;;
    esac
done <<< "$files"

# No beads data may land in the harness tree, in any repository. This is universal for the
# same reason `bash -n` is: it is not one repository's taste, and the cost of getting it
# wrong is unbounded — a database published in a shared repository holds internal working
# notes, agent memories and the overseer's own judgement, and cannot be un-published by
# deleting the commit (law-beads-is-never-public).
#
# It is the SECOND fence, and the one that matters. The first is a pre-commit hook, which
# `git commit --no-verify` bypasses by design and which a clone does not have armed until
# somebody runs `exclude.sh install`; neither of those is the committer's failing, and
# neither can be relied on. This runs on every branch before it merges and nothing here
# routes around it.
#
# The branch's WHOLE TREE, not its changed files: the question is what the repository would
# contain after landing, so a database that arrived on an earlier commit of the same branch
# is caught rather than waved through for not being in this diff. exclude.sh takes its
# scope from that same list, so it is answering about this ref and not about wherever the
# harness happens to sit in the shared checkout — and it exits 3, not 0, on a repository
# that carries no harness at all, which is most of them.
#
# And it fails CLOSED on its own absence. `bash <missing-file> | ...` prints to stderr and
# yields an empty offender list, which is indistinguishable from a clean tree — the exact
# shape where a check that could not run reports all-clear.
EXCLUDE="$(dirname "$0")/exclude.sh"
[ -r "$EXCLUDE" ] || verdict "$NV" missing-exclude "gate: $EXCLUDE is missing — refusing to land unchecked"
offenders="$(git -C "$REPO" ls-tree -r --name-only "$BR" 2>/dev/null \
    | bash "$EXCLUDE" filter 2>/dev/null)"
if [ -n "$offenders" ]; then
    verdict 1 beads-data "gate: $BR would land beads data in the harness tree:
$(printf '%s\n' "$offenders" | sed 's/^/gate:   /')
gate: a beads database is never public and belongs in no shared repository.
gate: remove them from the branch — there is no override for this one."
fi

# No branch may change a COPY of the harness in a repository that is not the harness's own.
# The service manager executes one tree; a second copy vendored into another repository is
# edited by correct work that then never runs, and nothing downstream can tell — the tree
# that was edited is self-consistent, so its suites are green, its gate is satisfied and its
# bead closes naming a real commit. Landed and in effect became different claims the moment
# there were two copies, and this is the only place that compares them
# (law-closed-is-not-landed, one layer out).
#
# Universal for the same reason the two above are: it is not one repository's taste, and it
# is the harness's own correctness at stake rather than the judged repository's.
#
# It fails CLOSED on its own absence, and skew.sh fails closed on its own confusion.
SKEW="$(dirname "$0")/skew.sh"
[ -r "$SKEW" ] || verdict "$NV" missing-skew "gate: $SKEW is missing — refusing to land unchecked"
skewout="$(bash "$SKEW" foreign "$REPO" "$BASE" "$BR" 2>&1)" || verdict 1 foreign-harness \
    "gate: $BR belongs in the harness's own repository, not $REPO_NAME.
$skewout"

# ---------------------------------------------------------------------------------------
# LAYER 2 — the repository's own gate. Empty means syntax was the whole trial.
# ---------------------------------------------------------------------------------------
CMD="$(repo_gate "$REPO_NAME")"
# A repository with no gate command of its own has been fully judged by the universal layer
# above, and passes. Through verdict() like everything else, so its meter line and its
# VERDICT= line exist — a caller must never have to tell "passed" from "exited early".
[ -n "$CMD" ] || verdict 0 syntax-only ""

# =======================================================================================
# D1 — THE VERDICT IS COMPUTED ONCE AND KEYED BY WHAT IT JUDGED.
#
# Every bead paid this gate at least twice: the aeon runs it before it closes, and the
# landing pass runs it again, on the same tree, minutes later. Measured across 51 sessions,
# 150 aeon runs; each full spira gate is 380-620s, and 1860s on one occasion. That is the
# whole of the DONE-to-LANDED latency and most of the contention that made a shared tree
# worth locking in the first place (sp-0v8).
#
# THE KEY IS THE QUESTION, NOT THE ASKER. Nothing before this identified a verdict at all,
# so nothing could be reused; gate-run.sh keyed its result to (branch commit, base commit),
# which every landing on the base invalidates, so under a moving base it restarted from zero
# forever (sp-j4ed). The key here is everything a verdict actually depends on:
#
#   the REPOSITORY         - whose gate it was, named rather than inferred from the rest.
#   the branch's TREE      - what the suites read. Two commits with identical content share
#                            a tree id, so an amend or a clean rebase reuses the verdict.
#   the CHANGED FILE LIST  - what a selective gate chooses its suites from.
#   the GATE COMMAND       - layer 2 in full.
#   THIS HARNESS           - gate.sh, exclude.sh and skew.sh are layer 1; a change to any of
#                            them changes what a pass means, and a cached pass from before it
#                            would be a verdict from a gate that no longer exists.
#
# THE BASE COMMIT IS DELIBERATELY NOT IN IT. This gate judges the BRANCH'S TREE, checked out
# detached and on its own, so the base's whole part in the trial is producing the changed-file
# list — and that list is in the key on its own account. Keying on the base commit as well
# would retire a verdict for a reason the trial never read: every landing in the repository
# moves it, so a branch gated twice without being touched — a retry, a second pass, a caller
# that already rebased — would pay in full for the identical trial.
#
# WHAT DOES RETIRE IT IS THE REBASE. A branch brought onto a base that has moved gets the
# base's content in its own tree, so the tree hash moves and the key moves with it. The two
# facts together are the rule: the verdict follows the tree that was judged, and nothing else.
#
# ONLY A PASS IS CACHED, DELIBERATELY. A FAIL is not a pure function of the tree — a flaky
# suite fails once and passes on the next run — and caching one would pin a flake to a branch
# permanently, which is a far worse failure than paying for a re-run. NO_VERDICT is never
# cached because retrying is its entire meaning, and BASE_FAIL is a fact about the base rather
# than about this key. So the cache can only ever save work, never create a wrong answer: the
# worst it can do is skip a gate that would have passed anyway.
#
# IT IS A PURE CACHE and may be deleted at any time.
# =======================================================================================
# THE METER IS DEFINED BEFORE EVERY WAY OUT THAT IT MEASURES, which now means before the
# verdict cache as well as before the lock. A meter only the runs which obtained the tree can
# write is blind to the two readings that matter most: the run that waited its whole budget
# and got nothing, and the run that skipped the tree entirely because the verdict already
# existed. The first version defined it below the cache check, so a reused verdict wrote no
# row at all and was indistinguishable in the log from a gate that was never called — the
# shape every silent-skip bug takes (law-absence-needs-a-positive-control).
# `waited=` and `ran=` start at zero so it is safe to call from any exit below this line.
GATE_LOG="${SPIRA_GATE_LOG:-$SPIRA_RUN/gate.log}"
GATE_WAITED=0
GATE_START=$(date +%s)
gate_meter() {           # gate_meter <exit-status> [<note>]
    printf '%s %s %s waited=%ss ran=%ss rc=%s%s\n' \
        "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$REPO_NAME" "$BR" \
        "$GATE_WAITED" "$(( $(date +%s) - GATE_START ))" "${1:-?}" "${2:+ $2}" >> "$GATE_LOG" 2>/dev/null
}

VERDICT_DIR="${SPIRA_VERDICTS:-$SPIRA_RUN/verdicts}"
gate_key() {
    local tree files_h cmd_h harness_h
    tree="$(git -C "$REPO" rev-parse --verify -q "$BR^{tree}" 2>/dev/null)" || return 1
    files_h="$(printf '%s' "$files"   | sha256sum | cut -d" " -f1)"
    cmd_h="$(  printf '%s' "$CMD"     | sha256sum | cut -d" " -f1)"
    # cat of the three, so a change in any one moves the key. Missing files are impossible
    # here — both were checked above — but `cat` failing would produce a stable empty hash
    # for every run, which is a cache that ignores the harness entirely, so it fails closed.
    harness_h="$(cat "$0" "$EXCLUDE" "$SKEW" 2>/dev/null | sha256sum | cut -d" " -f1)"
    [ -n "$harness_h" ] || return 1
    printf '%s\n' "$REPO_NAME $tree $files_h $cmd_h $harness_h" | sha256sum | cut -d" " -f1
}
GATE_KEY="$(gate_key || true)"

# A CACHED PASS IS RETURNED BEFORE THE TREE LOCK IS EVEN REACHED, which is the point: the
# second caller neither runs the suites nor queues for the worktree. It still emits its own
# VERDICT line and its own meter row, so a reused verdict is visible as one rather than
# looking like a gate that never ran.
#
# AND A VERDICT EXPIRES, because the key names everything the verdict depended on EXCEPT the
# box. The toolchain the command ran under, what was installed beside it, what the network
# answered: none of those are in the key, and all of them drift while it stands still. So a
# verdict is a claim about a moment as well as about a tree, and SPIRA_VERDICT_TTL is how
# long that moment lasts. The age is taken from the entry's own `at=`, not from its mtime,
# because a mtime is what any copy, restore or stray `touch` happens to leave behind.
#
# EXPIRY FAILS IN THE CHEAP DIRECTION, which for a cache is towards running the suites: an
# entry with no readable timestamp, or a TTL that is not a number, reads as expired. The
# worst that costs is one gate that would have been skipped, against a wrong pass that would
# be a branch landing on a trial nobody ran.
verdict_ttl="${SPIRA_VERDICT_TTL:-0}"
case "$verdict_ttl" in ''|*[!0-9]*) verdict_ttl=0 ;; esac

if [ -n "$GATE_KEY" ] && [ -r "$VERDICT_DIR/$GATE_KEY" ]; then
    # shellcheck disable=SC1090
    cached_when=""; cached_by=""; cached_at=""
    eval "$(sed -n 's/^\(when\|by\|at\)=\(.*\)$/cached_\1="\2"/p' "$VERDICT_DIR/$GATE_KEY" 2>/dev/null)"
    cached_age=-1
    case "$cached_at" in ''|*[!0-9]*) : ;; *) cached_age=$(( $(date +%s) - cached_at )) ;; esac
    if [ "$cached_age" -ge 0 ] && [ "$cached_age" -lt "$verdict_ttl" ]; then
        verdict 0 cached \
            "gate: this exact tree already passed $REPO_NAME's gate at ${cached_when:-an earlier time} (${cached_by:-unknown caller})
gate: key $GATE_KEY — same tree, same changed files, same command, same harness."
    fi
fi

# THE TREE IS THE BRANCH, never the shared checkout. `cd "$REPO"` would run the copy of
# these suites that is already installed on main — so a branch that breaks a guard would be
# tested by the guard it had not broken yet, and a branch that ADDS a suite would fail its
# own gate because the file does not exist on main yet.
#
# A DETACHED WORKTREE rather than a `git archive` extraction. The archive version could only
# afford to unpack `.claude`, which silently made this brain-shaped: no other repository's
# tests live there, and a gate that can only see one directory cannot run `cargo` or `npm`
# at all. A worktree shares the object store, so the full tree costs a checkout of the files
# that actually changed, and it is reused across passes.
TREE="$SPIRA_RUN/worktree/.gate.$(basename "$REPO")"

# SWEEP STALE GATE WORKTREES BEFORE OBTAINING THE LOCK. Old runs that were killed before
# cleanup (SIGKILL, power loss) leave registrations the lock check in the sweep skips safely.
SWEEP="$(dirname "$0")/gate-sweep.sh"
[ -r "$SWEEP" ] && bash "$SWEEP" "$REPO" >/dev/null 2>&1 || true

# ONE TREE PER REPOSITORY MEANS ONE TREE FOR ALL OF THAT REPOSITORY'S GATES, so the tree is
# LOCKED for the whole trial. Gates run concurrently by construction — every aeon runs one
# before it closes, and the landing pass runs one per branch it is about to merge — and
# without a lock each `checkout --detach` pulls the previous run's branch out from under it
# mid-command. The verdict is then about whatever was checked out last, and nothing says so:
# two consecutive runs both passed, one of them on a branch deliberately broken, because a
# concurrent gate had swapped the tree between them. That is the worst failure a gate has —
# it passes work it never looked at, and the landing pass acts on the pass.
#
# THE SIMPLE FIX, SHIPPED WITH THE METER THAT SAYS WHEN IT STOPS BEING ENOUGH. Serialising
# is correct at any scale; what it costs is wall clock, so every run records how long it
# waited for the tree and how long it then held it, and a non-zero wait is said out loud
# (law-take-the-simple-fix-with-a-meter). When that log shows waits approaching the runs
# themselves, the answer is a tree per branch — not a wider timeout.
#
# It fails CLOSED on a wait that runs out: a gate that could not obtain the tree has checked
# nothing, and "could not run" is not a pass.
mkdir -p "$(dirname "$TREE")"
spira_require flock || verdict "$NV" no-flock "gate: flock is not on PATH — refusing to run unserialised"
exec 9>"$TREE.lock" || verdict "$NV" no-lockfile "gate: cannot open the gate tree's lock at $TREE.lock"

# THE WAIT IS DERIVED FROM THE RUN, not picked. One holder can legitimately occupy the tree
# for two full gate timeouts — the branch's trial, then the same command against the base to
# establish whose fault a failure is — so a wait shorter than twice the timeout would time
# out against a single healthy holder and report that as a refusal.
#
# A CALLER THAT IS ITSELF ON A CLOCK MUST SET THIS DOWN to what it can afford. The default is
# longer than a landing pass's whole budget, and a gate cannot know its caller's deadline, so
# the caller passes one (`SPIRA_GATE_LOCK_WAIT`) rather than the gate guessing.
GATE_LOCK_WAIT="${SPIRA_GATE_LOCK_WAIT:-$(( ${SPIRA_GATE_TIMEOUT:-2700} * 4 ))}"
GATE_WAIT0=$(date +%s)
if ! flock -w "$GATE_LOCK_WAIT" 9; then
    # "NO VERDICT" IS ITS OWN EXIT STATUS, not a failure. A queued gate has judged nothing, so
    # a caller that cannot tell it apart from a red gate charges the wait to the branch: the
    # landing pass reopened the bead saying it "failed the landing gate", and three of those
    # poison a bead and escalate to the operator over a lock it never contended for. The
    # verdict is still withheld — a gate fails closed — but the blame is not the branch's.
    GATE_WAITED=$(( $(date +%s) - GATE_WAIT0 )); GATE_START=$(date +%s)
    verdict "$NV" lock-timeout \
        "gate: another gate has held $TREE for ${GATE_LOCK_WAIT}s — no verdict on $BR
gate: this is a queue, not a fault in the branch; retry, or raise SPIRA_GATE_LOCK_WAIT."
fi
GATE_WAITED=$(( $(date +%s) - GATE_WAIT0 ))
GATE_START=$(date +%s)
[ "$GATE_WAITED" -gt 0 ] && echo "gate: waited ${GATE_WAITED}s for $TREE" >&2

# EVERY OTHER WAY OUT IS COUNTED FROM THE EXIT TRAP — the pass, the branch's own failure, and
# the checkout that could not be proved. A meter only the happy path writes measures the
# happy path.

# gate_at <ref> -> 0 with TREE PROVEN to hold that ref's commit.
#
# The proof is the point. A checkout that reports success is not evidence the tree holds what
# was asked for — the lock above is what makes it true, and this is what says so out loud if
# it ever is not again (law-absence-needs-a-positive-control). A gate that cannot identify
# the tree it is about to judge must refuse rather than judge it.
gate_at() {
    local ref="$1" want have
    want="$(git -C "$REPO" rev-parse --verify -q "$ref^{commit}" 2>/dev/null)" || want=""
    [ -n "$want" ] || { echo "gate: cannot resolve $ref in $REPO_NAME" >&2; return 1; }
    if [ ! -e "$TREE/.git" ]; then
        # Through the chokepoint: one prune covers every worktree of the repository, so a
        # gate tidying up after itself must not be able to unregister the aeon whose branch
        # it is about to try.
        spira_prune_worktrees "$REPO" >/dev/null 2>&1
        git -C "$REPO" worktree add -q --detach "$TREE" "$ref" 2>/dev/null || {
            echo "gate: cannot create a gate worktree at $TREE" >&2; return 1; }
    else
        # `--force` because a previous gate may have left build output; `checkout --detach`
        # refuses nothing else here, and the lock makes the tree ours alone.
        git -C "$TREE" checkout -q --force --detach "$ref" 2>/dev/null || {
            echo "gate: cannot check $ref out in $TREE" >&2; return 1; }
    fi
    have="$(git -C "$TREE" rev-parse HEAD 2>/dev/null)"
    [ "$have" = "$want" ] && return 0
    echo "gate: $TREE is at ${have:-nothing}, not $ref ($want) — refusing to judge a tree it cannot identify" >&2
    return 1
}

# A TREE THE GATE CANNOT IDENTIFY IS A MACHINERY FAULT, not a branch fault. gate_at has
# already said on stderr which of the two ways it failed.
gate_at "$BR" || verdict "$NV" tree-unidentified ""

# THE GATE MUST NOT INHERIT THE HARNESS'S OWN CONFIGURATION.
# Tests run in an explicit, minimal environment. The sentinel service exports
# SPIRA_FAYTHS="builder ops"; systemd passed that into the gate, the gate passed it into
# test-fayth.sh, and two tests asserting the DEFAULT fayth list failed — so correct work was
# rejected because of a deployment setting made an hour earlier, and it would have been
# rejected again on every retry until the bead poisoned. A gate whose verdict depends on
# ambient environment is not deterministic, and a non-deterministic gate is worse than none:
# it rejects good work at random and teaches everyone to ignore it
# (law-gates-run-in-a-clean-environment).
#
# What a gate command MAY see is named here and nowhere else: where the branch came from,
# and what it changed. ~/.cargo/bin is on the path because a Rust repository's cheapest
# real gate is `cargo fmt --check` and lib.sh's PATH — written for systemd — has no toolchain
# on it.
#
# SPIRA_GATE_ALL is passed THROUGH rather than set, and defaults to 0. A repository whose gate
# selects its suites from the changed files needs a way to be told to run all of them anyway;
# leaking in from the environment only ever widens what is checked, which is the safe
# direction, and it is named here so that it is a seam rather than an ambient surprise.
FILELIST="$(mktemp)"; printf '%s\n' "$files" > "$FILELIST"
# The status is captured FIRST: `$?` inside a trap is whatever the previous command in the
# trap returned, so a cleanup line ahead of the meter would have every run recorded as the
# exit status of `rm`.
# THE TRAP IS THE BACKSTOP, NOT THE PATH. Every deliberate way out goes through verdict(),
# which disarms this first; what is left for the trap is a death nobody chose — a signal, or
# a bug that lets control fall off the end — and those must still be metered, as NO_VERDICT,
# because a gate that vanished judged nothing.
trap 'gate_rc=$?
     rm -f "${FILELIST:-}" 2>/dev/null
     [ -n "${TREE:-}" ] && [ -e "${TREE}/.git" ] && \
         git -C "${REPO:-/nonexistent}" worktree remove --force "$TREE" 2>/dev/null
     gate_meter "${gate_rc:-$NV}" died' EXIT
# 9>&- — THE TREE LOCK'S FD MUST NOT REACH THE GATE COMMAND. `exec 9>lock` leaves fd 9
# without close-on-exec, so every child inherits it, and flock is held as long as ANY holder
# of the descriptor lives. A repository's gate builds fixtures and can leave a server running;
# that server would then hold this repository's gate tree forever, and every later gate would
# wait out its whole budget and withhold its verdict — the lock turned from a queue into a
# deadlock by inheritance, which is exactly how the fixture lock failed within an hour of
# landing. Nothing below this line needs the descriptor: the wait is over and the parent
# holds it for the whole trial.
run_gate() {             # run_gate <ref-being-tested> -> the command's own status
    # THE OUTPUT IS NOT TRUNCATED. A tail -N here silently drops diagnostic from the
    # first failing step whenever enough subsequent output pushes it past the window —
    # a fixture builder that fails and emits its reason to stderr, followed by a full
    # suite run of one-line "ok" entries, loses the reason entirely. gate-spira.sh
    # already limits output: one line per passing suite, full output only for failures,
    # so total size is bounded without truncation here.
    ( cd "$TREE" && env -i \
        PATH="$HOME/.cargo/bin:$PATH" HOME="$HOME" TERM=dumb \
        SPIRA_GATE_REPO="$REPO" SPIRA_GATE_REPO_NAME="$REPO_NAME" \
        SPIRA_GATE_BRANCH="$1" SPIRA_GATE_BASE="$BASE" \
        SPIRA_GATE_FILES="$FILELIST" \
        SPIRA_GATE_ALL="${SPIRA_GATE_ALL:-0}" \
        SPIRA_BATCH_MAXPAR="${SPIRA_BATCH_MAXPAR:-}" \
        timeout "${SPIRA_GATE_TIMEOUT:-2700}" bash -c "$CMD" 9>&- ) 2>&1
    return $?
}

# CAPTURED FROM THE ASSIGNMENT, NEVER FROM AN `if`. `if out="$(...)"; then ...; fi` leaves
# `$?` holding the status of the *if statement*, which is 0 on the false branch — so every
# red read as "exited 0" and every classification below it was made on the wrong number.
out="$(run_gate "$BR")"; gate_rc_branch=$?
if [ "$gate_rc_branch" -eq 0 ]; then
    # RECORDED ONLY ON A PASS, and written through a temporary file so a caller that dies
    # mid-write cannot leave a half-file that reads as a valid verdict.
    if [ -n "$GATE_KEY" ]; then
        mkdir -p "$VERDICT_DIR" 2>/dev/null
        { printf 'when=%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
          # `at=` IS THE ONE THE GATE READS BACK — an epoch, because the age of a verdict is
          # arithmetic and `when=` is for whoever reads the file. Both are written from the
          # same moment; only this one decides whether the entry is still good.
          printf 'at=%s\n'   "$(date +%s)"
          printf 'by=%s\n'   "${SPIRA_GATE_CALLER:-$BR}"
          printf 'repo=%s\nbranch=%s\n' "$REPO_NAME" "$BR"
        } > "$VERDICT_DIR/.$GATE_KEY.$$" 2>/dev/null \
          && mv -f "$VERDICT_DIR/.$GATE_KEY.$$" "$VERDICT_DIR/$GATE_KEY" 2>/dev/null
    fi
    verdict 0 pass ""
fi

# A DEADLINE IS NOT A RED. `timeout` exits 124 when it kills the command, and gate-spira.sh
# prints nothing while suites pass — so a gate killed at its budget produced an EMPTY tail
# and arrived at the landing pass as a bare "this branch failed", with nothing in the bead
# note for the next aeon to act on (sp-p4rl). The budget is the machinery's, not the
# branch's: the full spira gate measured ~1860s against a 900s default (sp-snyj), so the
# common cause of a 124 here is a budget that is too small, which no branch can fix.
# A SILENT RED IS STILL A RED, and this is where the first attempt at this rule was wrong.
# Downgrading every red that printed nothing looks like the fix for sp-p4rl and is not: a
# perfectly ordinary gate command — `test`, a grep, a script that only speaks on success —
# fails silently and legitimately, and treating that as a machinery fault would let a
# genuinely broken branch retry forever and then page the operator about a lock that does not
# exist. sp-p4rl's actual case is the DEADLINE, handled by its own status immediately below.
#
# What the branch owes the next reader is the command and its status, so a silent red arrives
# as a rejection that can at least be reproduced rather than as an empty note.
[ -z "${out//[[:space:]]/}" ] && out="(the command printed nothing; it exited $gate_rc_branch)"

# WHICH CHECK REFUSED IT, for the yield record. A gate whose reds are mostly its own fault is
# deleted wholesale only if nobody can say WHICH part is at fault; named, one suite can be
# removed and the rest kept. The convention it reads is the one a repository gate already
# follows when it reports a failing suite by filename — a filename immediately followed by
# FAILED or by having been killed. Anything else leaves `-`: the reason slug is a true
# attribution and a guessed suite name is not.
GATE_SUITE="$(printf '%s\n' "$out" \
    | sed -n 's/.*[[:space:]]\([A-Za-z0-9._-]*\.sh\)[[:space:]]\(FAILED\|was killed\).*/\1/p' \
    | head -1)"
[ -n "$GATE_SUITE" ] || GATE_SUITE=-

if [ "$gate_rc_branch" -eq 124 ]; then
    verdict "$NV" timeout \
        "gate: $REPO_NAME's own gate was killed at ${SPIRA_GATE_TIMEOUT:-2700}s — it judged nothing.
gate: command: $CMD
gate: this is the harness's budget, not a fault in the branch; raise SPIRA_GATE_TIMEOUT.
$out"
fi

# A FAILING GATE MUST SAY WHOSE FAULT IT IS. A command that already fails against the base
# rejects every branch for a condition no branch caused — three attempts, then poison, then
# an escalation to the operator about work that was fine, with nothing in it pointing at the real
# cause. Measured on another: `cargo fmt --all -- --check` exits 1 against its
# own main, so declaring it as that repository's gate would have done exactly this.
#
# NOTHING LANDS EITHER WAY — a gate fails CLOSED, and "main is broken too" is not a licence
# to land unverified work. What changes is WHO IS CHARGED. Five times on 2026-09-06 a bead
# was reopened as "failed the gate" in the same breath as the gate's own output saying "it
# fails against origin/main too — this branch did not cause it" (sp-d21). BASE_FAIL is that
# sentence as a status the caller cannot overlook.
#
# THE BASE TRIAL IS ITSELF A CHECK THAT CAN FAIL TO RUN. If the base cannot be checked out,
# we do not know whose fault the red is — and guessing "the branch" is the bug being fixed.
base_ran=0; base_out=""
if gate_at "$BASE" >/dev/null 2>&1; then
    base_out="$(run_gate "$BASE" 2>&1)"; base_rc=$?
    # A base trial that TIMED OUT tells us nothing either; only a clean red on the base is
    # evidence the base is broken.
    [ "$base_rc" -ne 124 ] && base_ran=1
fi
gate_at "$BR" >/dev/null 2>&1

if [ "$base_ran" = 1 ] && [ "$base_rc" -ne 0 ]; then
    verdict "$SPIRA_GATE_BASEFAIL" base-red \
        "gate: $REPO_NAME's own gate failed: $CMD
$out
gate: it fails against $BASE too — this branch did not cause it.
gate: fix the repository, or clear that command from $SPIRA_REPO_MAP."
fi
if [ "$base_ran" = 0 ]; then
    verdict "$NV" base-untestable \
        "gate: $REPO_NAME's own gate failed: $CMD
$out
gate: and the same command could not be tried against $BASE, so whose fault this is
gate: cannot be established — refusing to charge it to the branch on a guess."
fi
verdict 1 branch-red \
    "gate: $REPO_NAME's own gate failed: $CMD
$out
gate: the same command passes against $BASE, so this is the branch's own."
