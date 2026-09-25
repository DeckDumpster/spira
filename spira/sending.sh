#!/usr/bin/env bash
#
# sending.sh — the Sending: send finished work out properly. Delete the branch and the
# worktree of every bead whose work has landed, and nothing else.
#
#   sending.sh                     one sending pass (what the sentinel runs)
#   sending.sh --dry-run           print each branch's disposition, change nothing
#   sending.sh <bead-id>           send exactly one bead's branch and worktree
#   sending.sh --status-from <f>   read `id<TAB>status` from a file instead of bd (tests)
#
# WHAT THIS REPLACES
# ------------------
# `gt convoy land` step 3, "cleans up polecat worktrees associated with the convoy's
# tracked issues". Steps 2, 4 and 5 — all-closed, close the convoy, notify — are
# pilgrimage.sh; this is the part that was left. The design ledger predicted this
# responsibility would vanish with polecat worktrees, and it did not: the aeon runner
# reintroduced worktrees on purpose, because the first supervised aeon checked its branch
# out in the shared tree and swept the interactive session's files into its commit. Every
# aeon therefore leaves a branch AND a worktree behind, and until now nothing removed
# either.
#
# THE DEFECT THIS EXISTS TO FIX, MEASURED
# ---------------------------------------
# CHECK 6 in the sentinel ended a successful land with `git branch -q -D "$br" 2>/dev/null`.
# git REFUSES to delete a branch that a worktree has checked out —
#
#     error: cannot delete branch 'feat' used by worktree at '/tmp/.../wt'
#
# — and the aeon's own worktree is always holding exactly that branch. So the delete could
# never succeed, the error went to /dev/null, and the branch survived. The next pass found
# the same closed bead with the same branch, merged it again (already up to date), pushed
# again (a no-op) and counted another action. From .runtime/spira/sentinel.log:
#
#     2026-09-05T06:29:33Z spira: ACT landed spira/sp-stranded
#     2026-09-05T06:31:35Z spira: ACT landed spira/sp-stranded
#
# That is not merely untidy. `acted` was never 0, and CHECK 8 — the judgement tier — fires
# only when `acted` is 0, so a harness that re-lands one branch forever can never notice it
# is starved. It is the same false-action bug already commented at CHECK 2, arriving by a
# different route: a cleanup step that does not verify its own effect reports success on
# the strength of having tried.
#
# So every deletion here is CHECKED AFTER THE FACT, and a branch that survives its own
# deletion is reported FAILED rather than counted.
#
# WHAT MAY BE DELETED
# -------------------
# Only a branch whose every change the repository's base ref already holds — either because
# the branch is an ancestor of it, or because merging the branch into it would produce the
# base's own tree. Never a tip comparison and never the bead's status
# (law-closed-is-not-landed): CLOSED is a claim about a database, and the question here is
# whether deleting this ref can lose work. If the remote is unreachable the fetch fails, the
# base ref is stale, and a freshly-landed branch reads as unlanded and is KEPT — the fetch
# failure biases toward hoarding branches, which is recoverable, rather than toward deleting
# work, which is not.
#
# The second reading is what a squashing repository needs, and it is not a relaxation. A
# squash lands the diff as one new commit the branch is not in the history of, so ancestry
# alone keeps that branch forever; and a branch that is never sent is re-examined on every
# pass, where the landing worker's rebase conflicts against changes already on the base and
# reopens the finished bead. Nothing about that repeats differently, so it repeats until
# someone deletes the ref by hand.
#
# An unlanded branch is never deleted, whatever its bead says. That is CHECK 6's problem to
# land or reopen, not this program's to tidy away.
set -uo pipefail
. "$(dirname "$0")/lib.sh"

# EVERY REPOSITORY THE HARNESS MANAGES, not one. A branch lives in the checkout the aeon
# cut it from, and an aeon's checkout now comes from its bead's `repo:` label — so a reaper
# that swept one repository would leave every other repository's landed branches and
# worktrees standing forever, and would report a clean pass while doing it. $REPO is set per
# repository by the sweep below; nothing here may read it before then.
WORKTREES="$SPIRA_RUN/worktree"
REPO=""
# THE REPOSITORY'S NAME, ALONGSIDE ITS PATH. A SENT line names a branch, and a branch name
# alone is ambiguous across a harness that manages seven repositories: `spira/sp-mebw` exists
# in exactly one of them, but nothing in the string says which, and the reader of the pane is
# being asked to go and look. Set beside REPO in sweep_repo so the two cannot disagree.
REPONAME=""

DRY=0; FETCH=1; ONLY=""; STATUS_FROM=""
while [ $# -gt 0 ]; do
    case "$1" in
        --dry-run)     DRY=1 ;;
        --no-fetch)    FETCH=0 ;;
        --status-from) STATUS_FROM="${2:?--status-from needs a file}"; shift ;;
        -*)            die "unknown flag: $1" ;;
        *)             ONLY="$1" ;;
    esac
    shift
done

say() { printf '%s\n' "$*"; }

# --------------------------------------------------------------------------------------
# Bead status and liveness both live in lib.sh now, at the one place a tree or a branch can
# be destroyed. They were here, and being here is what made them advisory: this file asked
# for two witnesses in PASS 1 and one in PASS 2, and every other deleter in the harness
# asked for none. A rule stated where the deletion happens cannot be skipped by a caller
# that took another route (see the DESTRUCTION section of lib.sh).
#
# `--status-from` is still the seam a suite drives the status witness through, and the
# honest manual entry point: it says exactly what the reaper believes about each bead.
# --------------------------------------------------------------------------------------
[ -n "$STATUS_FROM" ] && spira_status_seam "$STATUS_FROM"


sent=0; failed=0
LANDREF=""

# --------------------------------------------------------------------------------------
# send_branch <id> <branch> — remove the worktree, then the branch, then the remote branch,
# then verify. Order matters: the branch cannot be deleted while a worktree holds it, which
# is the entire bug this file exists for.
# --------------------------------------------------------------------------------------
send_branch() {
    local id="$1" br="$2" verb="${3:-SENT}" w held
    # Re-check liveness immediately before acting. The sentinel summons aeons in the same
    # pass that lands branches, so the gap between deciding and doing is a real window. Both
    # witnesses again, not just the pidfile: this recheck used to ask only `holder_alive`,
    # which is the witness with the documented blind spot.
    if held="$(spira_holder_witnesses "$id")"; then
        say "HELD   $id  $held (mid-send)"; return 0
    fi

    w="$(worktree_of "$br" "$REPO")"
    if [ -n "$w" ] && ! spira_destroy_worktree "$id" "$w" "$REPO" "landed in $LANDREF"; then
        say "FAILED $id  worktree $w was not removed — see $SPIRA_REAPLOG"
        failed=$((failed+1)); return 1
    fi

    # VERIFY. `git branch -D` failing silently is the whole reason this program exists; a
    # reaper that trusts its own exit status inherits the bug it was written to fix. The
    # verification is inside spira_destroy_branch, which re-reads the ref after the delete.
    SPIRA_DESTROY_ERR=""
    if ! spira_destroy_branch "$id" "$br" "$REPO" "landed in $LANDREF" sending; then
        [ "${SPIRA_DESTROY_ERR:-}" = "certified-queued" ] \
            && { say "SKIP   $id  CERTIFIED/BATCHED — waiting for verdict"; return 0; }
        say "FAILED $id  branch $br survived deletion: ${SPIRA_DESTROY_ERR:-refused, see $SPIRA_REAPLOG}"
        failed=$((failed+1)); return 1
    fi

    # Under `pr` the branch IS pushed, and under `push` only the landing branch is, so this
    # is defensive rather than routine. It is guarded on existence because a delete of a ref
    # that was never there is an error the caller would have to learn to ignore — and it
    # names the base's own remote rather than assuming `origin`.
    local rem; rem="$(ref_remote "$LANDREF")" || rem=""
    if [ -n "$rem" ] && git -C "$REPO" rev-parse --verify -q "$rem/$br" >/dev/null 2>&1; then
        spira_git_push "$REPO" -q "$rem" --delete "$br" 2>/dev/null \
            && say "  deleted $rem/$br"
    fi

    # AND THE `branch:` LABEL GOES WITH THE REF. The label records which branch a bead's
    # work lives on so a reopened bead returns to it (law-branch-affinity-is-recorded), and
    # that affinity is over the moment the branch is deleted: a bead reopened afterwards
    # would be handed the name of a ref that no longer exists, and an aeon that recreates it
    # from the base rebuilds a branch whose work already landed. Dropped only here, after the
    # ref is verifiably gone, so the label and the branch cannot disagree.
    bdq label remove "$id" "branch:$br" >/dev/null 2>&1 || true

    # The aeon's session log is deliberately KEPT. Sentinel CHECK 5 uses the existence of
    # .runtime/spira/<id>.log to tell a bead an aeon worked from one a human closed by hand,
    # so deleting it here would silently disable the closed-but-not-landed check for exactly
    # the beads that check exists for.
    sent=$((sent+1))
    say "$verb $id  $REPONAME $br"
}

# ======================================================================================
# THE SWEEP — one repository, both passes. Called once per repository the harness manages,
# with $REPO and $LANDREF set for it.
#
# The landing reference is per repository and is chosen by `spira_landref`, the one place
# that choice is made, so the ref a branch is CUT from and the ref it is judged against can
# never drift apart. It is not assumed to be `main` — three of the seven repositories here
# default to something else — and a repository whose answer cannot be established is skipped
# rather than swept against a ref that does not exist. Every landed test here answers "no"
# against a missing ref, which reads as "unlanded", so that failure was survivable in this
# file and catastrophic in CHECK 6; skipping loudly is what makes it visible in both.
# ======================================================================================
sweep_repo() {
    local name="$1" br id n w brs rem held
    REPO="$(repo_root "$name")" || { say "SKIP   $name  repo-map has no path for it"; return 0; }
    REPONAME="$name"

    if [ ! -e "$REPO/.git" ]; then say "SKIP   $name  $REPO is not a git checkout"; return 0; fi

    # ----------------------------------------------------------------------------------
    # PASS 1 — every spira/* branch gets a disposition. Exactly one of them.
    #
    # THE LOCAL QUESTION IS ASKED FIRST, AND THE FETCH IS PAID FOR ONLY IF THE ANSWER IS
    # YES. This runs inside every sentinel pass, two minutes apart, and now over seven
    # repositories rather than one — an unconditional fetch of each would be four thousand
    # round trips a day to GitHub to learn nothing about six repositories with no Spira
    # branch in them. Refs are a local read; ancestry is the only question that needs a
    # current base ref.
    # ----------------------------------------------------------------------------------
    brs="$(git -C "$REPO" for-each-ref --format='%(refname:short)' 'refs/heads/spira/*' 2>/dev/null)"
    LANDREF="$(spira_landref "$REPO")" || {
        say "SKIP   $name  cannot resolve the ref it lands on — give it a \`base\` in repo-map"
        return 0; }
    # The base's OWN remote, not a literal `origin`: a remote need not be called that, so
    # fetch was a quiet no-op in the one repository nothing else here refreshes.
    if rem="$(ref_remote "$LANDREF")" && [ "$FETCH" = 1 ] && [ -n "$brs" ]; then
        git -C "$REPO" fetch -q "$rem" 2>/dev/null
    fi
    for br in $brs; do
        id="${br#spira/}"
        [ -z "$ONLY" ] || [ "$ONLY" = "$id" ] || [ "$ONLY" = "$br" ] || continue

        # ONE WITNESS FUNCTION, the same one every deleter asks. Asked separately here, this
        # pass had two witnesses and no positive control on either — an unreachable database
        # answers "not in_progress" exactly as an open bead does, and that reads as permission.
        if held="$(spira_holder_witnesses "$id")"; then
            say "HELD   $id  $held"; continue
        fi
        # ANCESTRY, THEN CONTENT. A squashing repository lands the work as one new commit
        # the branch is not in the history of, so ancestry alone answers "unlanded" forever
        # about a branch whose changes are all on the base — and a branch that is never
        # sent is re-examined on every pass, which is how one merged bead was reopened
        # three times in thirteen minutes. content_landed asks whether merging this branch
        # would change anything; it is exact, local, and answers NO on a conflict, so it can
        # never authorise deleting a ref that still carries work.
        #
        # A SUPERSEDED BEAD'S BRANCH IS THE EXCEPTION. Its work landed under the successor's
        # id; merging this branch would conflict with changes already on the base, so
        # content_landed returns false — but leaving the branch standing re-exposes it to the
        # landing pass, which would otherwise have to skip it on every pass forever. Reap it
        # here. The same check landing.sh uses: both spellings of the supersession field,
        # because bd list and bd show name it differently (law-absence-needs-a-positive-control:
        # the first version read only the show spelling off a list row and the exemption never
        # fired once).
        if ! content_landed "$REPO" "$br" "$LANDREF"; then
            # THREE CHECKS BELOW ALL READ THE SAME BEAD. Fetch once, reuse via printf —
            # each bdjson call starts the binary fresh and costs ~0.5s on a cold path, so
            # three calls for one unlanded branch compound into a per-pass tax that pushed
            # this suite past the harness's slice (sp-8ekik).
            _bead_json="$(bdjson show "$id" 2>/dev/null)"
            if printf '%s\n' "$_bead_json" | python3 -c '
import sys, json
try: d = json.load(sys.stdin)
except Exception: sys.exit(1)
d = d if isinstance(d, list) else [d]
if not d: sys.exit(1)
sys.exit(0 if any((x.get("dependency_type") or x.get("type")) == "supersedes"
                  for x in (d[0].get("dependencies") or [])) else 1)' 2>/dev/null; then
                # SAFETY CHECK: the supersedes edge is a human or agent claim, not a
                # mechanical proof. If merge-tree exits 0 (no conflict), the branch adds
                # content absent from the base — a wrong or premature supersede mark, and
                # reaping would silently destroy that work. A non-zero exit (conflict) means
                # the base already holds the same content from the successor's commits, which
                # is the case this arm was written for. Only reap on a conflict; keep
                # otherwise and let landing.sh or a corrected supersede handle it. (sp-bxd0)
                #
                # n=0 GUARD: an empty branch cannot carry unlanded content, so merge-tree's
                # answer is meaningless — it trivially exits 0 on nothing. Skip the check
                # and reap directly.
                _sup_n="$(git -C "$REPO" rev-list --count "$LANDREF..$br" 2>/dev/null || echo '?')"
                if [ "${_sup_n:-?}" != "0" ] && git -C "$REPO" merge-tree --write-tree "$LANDREF" "$br" >/dev/null 2>&1; then
                    _sup_files="$(git -C "$REPO" diff --name-only "$LANDREF" "$br" 2>/dev/null | head -5)"
                    say "KEEP   $id  superseded but $_sup_n unlanded commit(s) add content absent from $LANDREF: ${_sup_files:-unknown files}"
                    # THE BRANCH STAYS; THE WORKTREE DOES NOT. spira_destroy_branch refuses a
                    # branch "checked out at <worktree>" regardless of whether the branch
                    # itself is safe to reap, so a KEPT branch's own worktree stands forever
                    # once its aeon exits — the worktree was never what made this branch
                    # unsafe. The holder witness at the top of this loop already established
                    # no live aeon is inside it, so freeing the worktree here costs nothing
                    # and lets a later correction (a fixed supersede mark, a content_landed
                    # fix) reap the branch without a hand clearing worktrees first.
                    _sup_w="$(worktree_of "$br" "$REPO")"
                    if [ -n "$_sup_w" ]; then
                        if [ "$DRY" = 1 ]; then
                            say "WOULD  $id  free worktree $_sup_w (branch kept)"
                        else
                            spira_destroy_worktree "$id" "$_sup_w" "$REPO" "branch $id kept as superseded; worktree freed"
                        fi
                    fi
                    continue
                fi
                if [ "$DRY" = 1 ]; then
                    say "WOULD  $id  reap superseded branch $br$( [ -n "$(worktree_of "$br" "$REPO")" ] && printf ' and its worktree')"
                    continue
                fi
                send_branch "$id" "$br" "REAPED"
                continue
            fi
            # SQUASH-MERGED CLOSED BEAD. A squash lands the work as one commit the branch is
            # not an ancestor of. content_landed then answers NO about a finished branch whose
            # changes are all on the base — specifically when post-squash commits on the base
            # touch the same files and produce a conflict on merge-tree. The conflict means the
            # base holds the squashed content AND something beyond it; the branch carries
            # nothing the base lacks.
            #
            # A merged PR whose headRefOid still matches the current branch tip is proof that:
            # (a) every commit on this branch was captured by the PR at the moment of the
            # squash-merge, and (b) no commits were added to the branch after the merge, so
            # the ref holds nothing further. Together with a CLOSED bead these three facts
            # constitute the same assurance content_landed gives for a fast-forward: the work
            # is on the base and the ref can be reaped safely.
            #
            # pr_merged alone is never evidence for deletion (its own comment says so), but
            # pr_merged AND unchanged-tip AND closed-bead together are. The tip check is what
            # makes it safe: without it, commits added after the squash would be silently
            # destroyed. With it, those commits are impossible by definition.
            #
            # NETWORK CALL. ghq reaches GitHub, so this runs only after content_landed and
            # superseded have both said no — never on the common path.
            if printf '%s\n' "$_bead_json" | python3 -c '
import sys, json
try: d = json.load(sys.stdin)
except Exception: sys.exit(1)
d = d if isinstance(d, list) else [d]
sys.exit(0 if d and d[0].get("status") == "closed" else 1)' 2>/dev/null; then
                _br_tip="$(git -C "$REPO" rev-parse "$br" 2>/dev/null)"
                _pr_tip="$(cd "$REPO" 2>/dev/null && ghq pr view "$br" \
                    --json state,headRefOid \
                    -q 'select(.state=="MERGED") | .headRefOid' 2>/dev/null)"
                if [ -n "$_pr_tip" ] && [ "${_br_tip:-}" = "$_pr_tip" ]; then
                    if [ "$DRY" = 1 ]; then
                        say "WOULD  $id  reap squash-merged branch $br (PR merged at this tip)"
                        continue
                    fi
                    send_branch "$id" "$br" "REAPED"
                    continue
                fi
            fi
            n="$(git -C "$REPO" rev-list --count "$LANDREF..$br" 2>/dev/null || echo '?')"
            # FAST-FORWARD MERGED. After a push-mode landing the base advances to the branch
            # tip, so the branch has zero commits ahead and IS an ancestor of the base.
            # content_landed returns non-zero for all zero-ahead branches (cannot tell landed
            # from empty without a positive control), but a commit on the base that names the
            # bead IS that control (law-landed-is-content, law-absence-needs-a-positive-control).
            # Reap it. No content-landed label is needed — CHECK 5 finds the commit itself.
            # An empty branch falls through: no such commit means landed() returns non-zero.
            if [ "${n:-?}" = 0 ] \
               && git -C "$REPO" merge-base --is-ancestor "$br" "$LANDREF" 2>/dev/null; then
                if landed "$id" "$REPO" 2>/dev/null; then
                    if [ "$DRY" = 1 ]; then
                        say "WOULD  $id  send branch $br (zero ahead, commit on $LANDREF names it)"
                        continue
                    fi
                    send_branch "$id" "$br"
                    continue
                fi
                # NON-CODE DELIVERS: a bead that declared a non-code deliverable
                # was never expected to commit. 0 ahead + ancestor means nothing to protect;
                # the delivers: label is the positive control that says the empty branch is
                # intentional rather than an aeon that failed to commit (sp-v4652).
                # Works for both OPEN and CLOSED beads (OPEN beads with 0 commits can have
                # non-code deliverables like sp-5lvh5).
                if printf '%s\n' "$_bead_json" | python3 -c '
import sys, json
try: d = json.load(sys.stdin)
except Exception: sys.exit(1)
d = d if isinstance(d, list) else [d]
if not d: sys.exit(1)
b = d[0]
labs = b.get("labels") or []
has_delivers = any(str(l).startswith("delivers:") for l in labs)
sys.exit(0 if has_delivers else 1)' 2>/dev/null; then
                    if [ "$DRY" = 1 ]; then
                        say "WOULD  $id  reap non-code-delivers branch $br (closed, 0 ahead, no commit expected)"
                        continue
                    fi
                    send_branch "$id" "$br" "REAPED"
                    continue
                fi
                # OPEN BEAD WITH ZERO AHEAD: an open bead with n=0 that is an ancestor of
                # the landing ref has its work already on main. Even if landed() did not find
                # a commit explicitly naming the bead, the branch itself is reachable from the
                # base, so reap it. This handles cases where work lands through a batch commit
                # or other mechanism that does not name the individual bead.
                if ! printf '%s\n' "$_bead_json" | python3 -c '
import sys, json
try: d = json.load(sys.stdin)
except Exception: sys.exit(1)
d = d if isinstance(d, list) else [d]
sys.exit(0 if d and d[0].get("status") == "closed" else 1)' 2>/dev/null; then
                    if [ "$DRY" = 1 ]; then
                        say "WOULD  $id  reap branch $br (open, 0 ahead, already on $LANDREF)"
                        continue
                    fi
                    send_branch "$id" "$br" "REAPED"
                    continue
                fi
            fi
            # LANDED BY OTHER PR. A batch commit that names this bead satisfies landed()
            # even when the bead's own PR was closed unmerged and the branch conflicts with
            # the base. Ask here, not only at n=0 (law-closed-is-not-landed, sp-v4652).
            # Works for OPEN beads with n=0 as well (sp-5lvh5).
            #
            # landed() ALONE IS NOT ENOUGH TO DELETE. It answers "a landing record names
            # this bead", not "this branch's own commits are the ones that landed" — a
            # correct record for a PRIOR push of this same branch stays true forever, even
            # after the branch gained commits that record never saw. `git cherry` asks the
            # question content_landed asks but per-commit: every commit unique to $br must
            # already be patch-equivalent to something on $LANDREF ('-'), or this reap
            # deletes real, unlanded work under a landed() that is true about the past
            # (sp-dgaig).
            if printf '%s\n' "$_bead_json" | python3 -c '
import sys, json
try: d = json.load(sys.stdin)
except Exception: sys.exit(1)
d = d if isinstance(d, list) else [d]
sys.exit(0 if d else 1)' 2>/dev/null \
               && landed "$id" "$REPO" 2>/dev/null; then
                # NO -q: grep -q closes the pipe at its first match, and git cherry then
                # dies of SIGPIPE — under pipefail that reads as "nothing found" exactly when
                # something was (law-no-grep-q-under-pipefail). grep without -q drains the
                # whole pipe every time, so git cherry always exits on its own.
                _cherry_plus="$(git -C "$REPO" cherry "$LANDREF" "$br" 2>/dev/null | grep '^+')"
                if [ -n "$_cherry_plus" ]; then
                    say "KEEP   $id  $n commit(s) not in $LANDREF; landed() names it but git cherry finds unapplied commits — not safe to reap"
                    continue
                fi
                if [ "$DRY" = 1 ]; then
                    say "WOULD  $id  send branch $br (commit on $LANDREF names it)"
                    continue
                fi
                send_branch "$id" "$br"
                continue
            fi
            if printf '%s\n' "$_bead_json" | python3 -c '
import sys, json
try: d = json.load(sys.stdin)
except Exception: sys.exit(1)
d = d if isinstance(d, list) else [d]
sys.exit(0 if d and "status" in d[0] else 1)' 2>/dev/null; then
                say "KEEP   $id  unlanded — $n commit(s) not in $LANDREF"
            else
                say "UNADOPTED $id  no bead — $n commit(s) not in $LANDREF"
            fi
            continue
        fi
        if [ "$DRY" = 1 ]; then
            say "WOULD  $id  send branch $br$( [ -n "$(worktree_of "$br" "$REPO")" ] && printf ' and its worktree')"
            continue
        fi
        # LABEL BEFORE DELETING, AND ONLY WHEN THERE WAS WORK TO LAND. This branch is about
        # to disappear, and with it the only evidence CHECK 5 has that the bead was not simply
        # closed on nothing: it searches the base for a commit naming the id, and a content
        # reap makes no such commit. Without the label it reopens the bead a pass later and a
        # fresh aeon redoes finished work — 99 reopens over 80 beads in a day (sp-796o).
        #
        # AHEAD-COUNT FIRST, because content_landed is also true for a branch carrying nothing.
        # An aeon that closes its bead having committed no working change leaves a branch with
        # zero commits ahead of the base, merging it produces the base tree, and labelling that
        # would exempt the empty case from the one check that exists to catch it. Zero ahead is
        # either a real fast-forward merge, whose commit names the bead so CHECK 5 is satisfied
        # anyway, or empty work that SHOULD be reopened. Only a branch with commits of its own
        # whose diff is nonetheless already on the base is the case this label describes.
        _ahead="$(git -C "$REPO" rev-list --count "$LANDREF..$br" 2>/dev/null || echo 0)"
        if [ "${_ahead:-0}" -gt 0 ] 2>/dev/null; then
            bdq label add "$id" content-landed >/dev/null 2>&1 || true
        fi
        # ASSERT: every branch content_landed confirms as done should have been seen by
        # landing.sh first. landing.sh writes $SPIRA_RUN/landstate/$id on every code path
        # that processes a branch — gate, rebase conflict, or the actual push — so a missing
        # record means the branch slipped past landing.sh's selection entirely. That is the
        # shape of sp-qj8n: four close/reopen cycles, no landstate entry on any of them,
        # because landing.sh's scan never included the branch. Logging here flags the anomaly
        # on the FIRST reap rather than after the reopen cycles that diagnosed the original.
        # The send proceeds regardless — cleanup must not stall on a diagnostic.
        if [ ! -f "${SPIRA_RUN}/landstate/$id" ]; then
            log "sending: ASSERT $id — content landed but no landstate/$id; landing.sh may not have selected this branch (sp-qj8n shape)"
        fi
        send_branch "$id" "$br"
    done

    # ----------------------------------------------------------------------------------
    # PASS 2 — orphaned worktrees. A worktree outlives its branch whenever a send is
    # interrupted between the two deletions, and an orphan is not inert: aeon.sh reuses any
    # directory that already looks like a worktree, so the next aeon for that bead would be
    # handed a checkout of a branch that no longer exists.
    # ----------------------------------------------------------------------------------
    [ -n "$ONLY" ] && return 0
    while IFS= read -r w; do
        [ -n "$w" ] || continue
        case "$w" in "$WORKTREES"/*) ;; *) continue ;; esac   # never the shared checkout
        # The harness's own trees are named with a leading dot and are permanent: .landing.*
        # lands, .rebase.* replays, .gate.* is where a branch stands trial. Listing them
        # individually was fine while there was one of each; there is now one of each PER
        # REPOSITORY, and a skip-list that has to be extended by hand is a skip-list that
        # eventually deletes the tree the next pass needed.
        case "$(basename "$w")" in .*) continue ;; esac
        id="$(basename "$w")"
        # Only branch-backed worktrees are ours to judge; a detached one under this
        # directory is something a human made and is left alone.
        br="$(git -C "$REPO" worktree list --porcelain 2>/dev/null | python3 -c '
import sys
want = sys.argv[1]; path = None
for line in sys.stdin:
    line = line.rstrip("\n")
    if line.startswith("worktree "): path = line[9:]
    elif line.startswith("branch ") and path == want:
        print(line[7:].removeprefix("refs/heads/")); break
' "$w" 2>/dev/null)"
        [ -n "$br" ] || continue
        git -C "$REPO" show-ref --verify -q "refs/heads/$br" && continue   # branch still exists
        # THE SAME TWO WITNESSES AS PASS 1. This asked only `holder_alive` — the one guard in
        # the file weaker than the rule the file states — and a missing branch is not evidence
        # that nobody is home: an aeon works for its first minutes with a branch that exists
        # only locally, and any prune that unregistered a live tree produces exactly this
        # shape. spira_destroy_worktree refuses on either witness and salvages before acting.
        if held="$(spira_holder_witnesses "$id")"; then
            say "HELD   $id  orphaned worktree kept — $held"; continue
        fi
        if [ "$DRY" = 1 ]; then say "WOULD  $id  remove orphaned worktree $w"; continue; fi
        spira_destroy_worktree "$id" "$w" "$REPO" "orphan: branch $br is gone" || {
            say "FAILED $id  orphaned worktree $w was not removed — see $SPIRA_REAPLOG"
            failed=$((failed+1)); continue; }
        sent=$((sent+1))
        say "SENT $id  $REPONAME $br  orphaned worktree (branch was already gone)"
    done < <(git -C "$REPO" worktree list --porcelain 2>/dev/null | sed -n 's/^worktree //p')

    # Registrations whose directory a human already deleted. Harmless, but they accumulate
    # and make `git worktree list` unreadable, which is how the real ones get missed.
    # Through the chokepoint, which repairs rather than prunes any entry whose directory is
    # still on disk, and names in the reap log every entry it really does drop.
    [ "$DRY" = 1 ] || spira_prune_worktrees "$REPO"
    return 0
}

# ======================================================================================
# THE LEGACY TREES. `.landing` and `.rebase` were single, unsuffixed and registered against
# whichever repository happened to create them; they are `.landing.<repo>` and
# `.rebase.<repo>` now. The old pair cannot serve a second repository and nothing will ever
# check anything out in them again, so retire them once rather than leaving two permanent
# registrations that make `git worktree list` unreadable.
# ======================================================================================
# These are DETACHED scratch trees the harness made for itself and no bead was ever worked in
# one, so the liveness witnesses will always say nobody is home — but they go through the
# chokepoint anyway rather than being exempted from it. An exemption is a second code path,
# and a second code path is what this whole section is a response to.
[ "$DRY" = 1 ] || for legacy in "$WORKTREES/.landing" "$WORKTREES/.rebase"; do
    [ -e "$legacy" ] || continue
    for r in $(spira_repos); do
        rp="$(repo_root "$r")" || continue
        spira_destroy_worktree "$(basename "$legacy")" "$legacy" "$rp" \
            "legacy harness tree, superseded by the per-repository one" && break
    done
    [ -e "$legacy" ] && continue     # nothing could remove it: say nothing rather than lie
    say "RETIRED $(basename "$legacy")  superseded by the per-repository tree"
done

for repo_name in $(spira_repos); do
    sweep_repo "$repo_name"
done

[ "$DRY" = 1 ] || log "sending: $sent sent, $failed failed"
[ "$failed" -eq 0 ]
