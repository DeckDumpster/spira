#!/usr/bin/env bash
#
# branch-guard.sh — an aeon must commit to its worktree branch, never to the base branch.
#
#   branch-guard.sh staged [root]   pre-commit entry point
#   branch-guard.sh check  [root]   report aeon commits on the base branch and checkouts
#                                   that are ahead of their remote
#
# WHY THIS EXISTS
# ---------------
# An aeon works in a worktree of the shared checkout on its own branch. If it cd-s back
# to the shared checkout and commits there, the commit lands on the base branch — bypassing
# its worktree branch, the landing gate, and every instrument that reads refs/heads/spira/*:
# the landing check, the Sending, and the unsent count are all blind to it.
#
# The path that produces the defect: aeon.sh sets the worktree at $SPIRA_RUN/worktree/$BEAD_ID
# and cd-s into it. An aeon that ran `git commit` in its worktree would have committed on its
# own branch, as intended. One that `cd`-ed out of the worktree back to the shared $REPO and
# committed there landed on whatever branch the shared checkout had, typically the base branch.
# Nothing in the commit path at that point knew it was wrong.
#
# A FENCE, NOT A WALL. Ryan and the brain session commit to the shared checkout directly on
# purpose — the base branch has many operator commits in its history. The aeon identity is
# what makes a commit wrong here, not the path it arrived through. An aeon's committer email
# is always "aeon-<name>@spira.local", set by aeon.sh; that suffix is the discriminator, and
# it is one that no human on this box carries.
#
# THE OVERRIDE is "git commit --no-verify", which git already provides and which there is
# no point pretending to take away. The fence that survives --no-verify is the landing gate,
# which runs inventory.sh and the other configured fences on every branch.
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/lib.sh"

# is_aeon_email <email> -> 0 if this email identifies an aeon committer.
#
# The suffix is the discriminator, not the name — different aeons carry different names and
# the guard must bind all of them. An operator committing from the same checkout has a normal
# email address that does not end in @spira.local, so the check is a single glob test.
is_aeon_email() {
    case "${1:-}" in *@spira.local) return 0 ;; *) return 1 ;; esac
}

cmd="${1:-staged}"; shift || true
ROOT="${1:-$(git rev-parse --show-toplevel 2>/dev/null || echo .)}"

case "$cmd" in

# ---------------------------------------------------------------------------------------
# staged — the pre-commit entry point.
#
# Called by the pre-commit hook after exclude.sh staged. Refuses when:
#   - the committer email ends in @spira.local (aeon identity), AND
#   - the current branch is this repository's base branch (main / master / etc.)
#
# The aeon check is first and cheap — most commits skip this whole guard without asking
# git anything.
# ---------------------------------------------------------------------------------------
staged)
    # NOT AN AEON: nothing to check. Most commits exit here.
    is_aeon_email "${GIT_COMMITTER_EMAIL:-}" || exit 0

    # WRONG WORKTREE. If the aeon was assigned a worktree (SPIRA_WORK is set by aeon.sh),
    # any commit from a different directory is refused. The check compares ROOT — the git
    # toplevel of wherever `git commit` is running — against the assigned path.
    #
    # This is the rung-4 mechanism for law-worktrees-in-the-sanctioned-root: a statute (rung
    # 3) already says to cut worktrees only under $SPIRA_RUN/worktree; this makes committing
    # outside the assigned one structurally impossible rather than merely advisory. The defect
    # it prevents: aeon-yojimbo committed to the production checkout directly, creating a
    # commit that existed only there, diverged from origin/main, and froze the promote path.
    #
    # NOT a wall. Override: git commit --no-verify  (the landing gate still runs inventory.sh).
    if [ -n "${SPIRA_WORK:-}" ] && [ "$ROOT" != "$SPIRA_WORK" ]; then
        {
            printf '\n'
            printf 'REFUSED by branch-guard.sh — an aeon must commit in its assigned worktree.\n'
            printf '\n'
            printf '  committer   : %s <%s>\n' "${GIT_COMMITTER_NAME:-}" "${GIT_COMMITTER_EMAIL:-}"
            printf '  this tree   : %s\n' "$ROOT"
            printf '  assigned to : %s  (SPIRA_WORK)\n' "$SPIRA_WORK"
            printf '\n'
            printf 'Commit in the worktree: git -C "$SPIRA_WORK" commit ...\n'
            printf 'Override: git commit --no-verify   (the landing gate still runs)\n'
            printf '\n'
        } >&2
        exit 1
    fi

    # The current branch. A detached HEAD is not the base branch, so let the commit proceed.
    current="$(git -C "$ROOT" symbolic-ref --short HEAD 2>/dev/null)" || exit 0

    # Resolve the base branch for this repository. If it cannot be determined, fail closed:
    # an aeon commit whose target branch cannot be verified is the same risk as one we can
    # confirm is the base, and the cost of one refused commit is lower than one wrong merge.
    landref="$(spira_landref "$ROOT" 2>/dev/null)" || {
        {
            printf '\n'
            printf 'REFUSED by branch-guard.sh — committer is an aeon identity and the base branch\n'
            printf 'for %s could not be resolved.\n' "$ROOT"
            printf '\n'
            printf '  committer : %s <%s>\n' "${GIT_COMMITTER_NAME:-}" "${GIT_COMMITTER_EMAIL:-}"
            printf '\n'
            printf 'Use the worktree: git -C $WORK commit ...\n'
            printf 'Override: git commit --no-verify   (the landing gate still runs)\n'
            printf '\n'
        } >&2
        exit 1
    }
    base="$(ref_branch "$landref")"
    [ "$current" = "$base" ] || exit 0  # on a feature branch — fine

    {
        printf '\n'
        printf 'REFUSED by branch-guard.sh — an aeon may not commit to the base branch.\n'
        printf '\n'
        printf '  committer : %s <%s>\n' "${GIT_COMMITTER_NAME:-}" "${GIT_COMMITTER_EMAIL:-}"
        printf '  repository: %s\n' "$ROOT"
        printf '  branch    : %s  (this is the base branch)\n' "$current"
        printf '\n'
        printf 'An aeon works in a worktree on its own branch. A commit here bypasses the\n'
        printf 'gate and is invisible to every instrument that reads refs/heads/spira/* —\n'
        printf 'the landing check, the Sending, and the unsent count all miss it.\n'
        printf '\n'
        printf 'Use the worktree: git -C $WORK commit ...   ($WORK is set in your session)\n'
        printf '\n'
        printf 'Override: git commit --no-verify   (the landing gate still runs)\n'
        printf '\n'
    } >&2
    exit 1
    ;;

# ---------------------------------------------------------------------------------------
# check — the standing audit, run on a timer.
#
# Two cheap reads per registered repository:
#
#   1. Is the base branch's tip a non-merge commit by an aeon identity? A merge commit is
#      the normal landing shape; a non-merge aeon commit is the exact defect this file was
#      written to stop.
#
#   2. Is the shared checkout ahead of its remote? A commit that landed without a push
#      bases every subsequent aeon's worktree on a ref nobody else has seen yet.
#
# Prints nothing and exits 0 when everything is clean. Prints findings and exits 1 when
# either anomaly is present. Broken reads render a note rather than silence, because a probe
# that cannot answer is not the same as one that answered clean (law-absence-needs-a-positive-control).
# ---------------------------------------------------------------------------------------
check)
    bad=0; any=0
    for rname in $(spira_repos 2>/dev/null); do
        root="$(repo_root "$rname" 2>/dev/null)" || continue
        [ -d "$root/.git" ] || continue
        any=1
        landref="$(spira_landref "$root" 2>/dev/null)" || {
            printf 'branch-guard: %s — could not resolve base branch\n' "$rname"
            continue
        }
        base="$(ref_branch "$landref")"

        # 1. Is the tip of the local base branch a non-merge aeon commit?
        #
        # THREE-DOT ANCESTRY IS NOT NEEDED HERE: the question is about the branch TIP, not
        # the diff, so a single `git log -1` is the right read. Merge commits have two
        # parents; a direct aeon commit has one. --format%P is the parent SHA list, which is
        # empty for an initial commit and two hashes for a merge — a word count of the field
        # tells them apart without interpreting SHA strings.
        tip_email="$(git -C "$root" log -1 --format=%ae "refs/heads/$base" 2>/dev/null)" || tip_email=""
        # wc -w on an empty string returns 0, on one SHA returns 1, on two returns 2.
        tip_nparents="$(git -C "$root" log -1 --format=%P "refs/heads/$base" 2>/dev/null \
                       | wc -w)"
        if [ -n "$tip_email" ] && is_aeon_email "$tip_email" \
               && [ "${tip_nparents:-0}" -lt 2 ]; then
            sha="$(git -C "$root" rev-parse --short "refs/heads/$base" 2>/dev/null)"
            subj="$(git -C "$root" log -1 --format=%s "refs/heads/$base" 2>/dev/null)"
            printf 'branch-guard: AEON COMMIT ON %s/%s — %s by <%s>: %s\n' \
                   "$rname" "$base" "${sha:-?}" "$tip_email" "${subj:-(no subject)}"
            bad=1
        fi

        # 2. Is the shared checkout ahead of its remote?
        remote="$(ref_remote "$landref" 2>/dev/null)" || remote=""
        if [ -n "$remote" ]; then
            ahead="$(git -C "$root" rev-list --count \
                         "${remote}/${base}..refs/heads/${base}" 2>/dev/null)" || ahead="?"
            case "${ahead}" in
                ''|0) ;;
                '?') printf 'branch-guard: %s — could not count commits ahead of %s/%s\n' \
                           "$rname" "$remote" "$base" ;;
                *) printf 'branch-guard: SHARED CHECKOUT AHEAD — %s is %s commit(s) ahead of %s/%s\n' \
                          "$rname" "$ahead" "$remote" "$base"
                   bad=1 ;;
            esac
        fi
    done

    if [ "$any" = 0 ]; then
        printf 'branch-guard: no registered repositories — nothing to audit\n' >&2
        exit 3
    fi

    [ "$bad" = 0 ] \
        && printf 'branch-guard: clean — no aeon commits on base branches, no checkouts ahead of remote\n'
    exit "$bad"
    ;;

*)
    sed -n '3,8p' "$0" >&2
    exit 2
    ;;
esac
