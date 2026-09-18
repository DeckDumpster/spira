#!/usr/bin/env bash
#
# skew.sh — is the ACTIVATED release the latest published one?
#
#   skew.sh check [--escalate]             audit this box; escalate on findings (only with --escalate)
#   skew.sh units                          are the installed units what the templates render?
#   skew.sh refresh [repo]                 fast-forward the checkout to its base ref
#   skew.sh copies                         every mapped repository carrying a harness copy
#   skew.sh foreign <repo> <base> <ref>    may this branch land? — the landing gate's fence
#
# WHAT THIS IS FOR
# ----------------
# The installed Spira is read-only: the only way to change it is to activate a new release
# tarball. A drift check against a tree that cannot drift reports clean forever, which is
# indistinguishable from a check that is broken. So check asks the questions that exist:
#
#   NOT-LATEST        the activated release is not the most recent published release tag.
#                     A newer tarball was activated elsewhere, or this box was skipped.
#   MANIFEST-MISMATCH the MANIFEST commit in the activated release does not match the commit
#                     the corresponding release tag points at. The artifact and the tag
#                     disagree about what commit built this release.
#
# Both are answerable from the artifact and the git tag — no working tree comparison, no
# fetch required beyond what the tag list returns.
#
# `check` REPORTS; it does not repair. Activation is activate.sh's job.
#
# EXIT   0  checked, and the activated release is the latest; MANIFEST matches its tag
#        1  checked, and it is not — the finding is on stdout; with --escalate it has been escalated
#        3  could not check — said out loud, never a silent pass
#              (law-absence-needs-a-positive-control)
set -uo pipefail
# AN INITIALIZATION FAILURE IS "COULD NOT CHECK", EXIT 3, NOT A FOREIGN-HARNESS VERDICT.
# conf.sh calls `exit 1` when `bd migrate schema` fails (e.g., database locked). gate.sh
# calls this script as `skew.sh foreign` and treats any non-zero exit as a foreign-harness
# refusal — so a database lock produced the message "branch belongs in the harness's own
# repository" with the actual cause buried in the captured output. The fence fails closed
# on its own confusion (stated at line 78), but it must name the confusion, not announce
# a violation that never happened. Exit 3 is the documented "could not check" code; the
# trap is removed once lib.sh has been sourced successfully so it cannot mask later exits.
_skew_init_done=0
trap '[ "$_skew_init_done" = 0 ] && exit 3' EXIT
. "$(dirname "$0")/lib.sh"
_skew_init_done=1
trap - EXIT

EXCLUDE="$(dirname "$0")/exclude.sh"

# harness_in <repo-path> -> the harness directories in its WORKING TREE, one per line.
# Exit 1 when it holds none, which is the ordinary answer for most repositories.
harness_in() {
    local root="$1"
    [ -e "$root/.git" ] || return 1
    git -C "$root" ls-files 2>/dev/null | bash "$EXCLUDE" harness-in 2>/dev/null
}

# harness_in_ref <repo-path> <ref> -> the harness directories in that REF, one per line.
# The ref and not the checkout: the question a landing gate asks is what the repository would
# contain after this branch merges, and the branch may be the thing that adds the copy.
harness_in_ref() {
    local root="$1" ref="$2"
    git -C "$root" ls-tree -r --name-only "$ref" 2>/dev/null | bash "$EXCLUDE" harness-in 2>/dev/null
}

# =======================================================================================
# foreign — the landing gate's fence.
#
# A branch in a repository that is NOT the harness's own may not change files inside a copy
# of the harness. Correct work landing there is work nothing will execute, and the aeon that
# wrote it has no way to tell: it committed, on a branch, naming its bead, and every suite in
# the tree it edited passed.
#
# IT FAILS CLOSED, including on its own confusion. A fence that cannot tell whether the rule
# was broken must not answer "not broken" — that is the one output a broken fence produces
# every time.
#
# IT IS SCOPED TO THE COPY, NOT TO THE REPOSITORY. A repository may legitimately carry a
# vendored harness and still have a thousand files of its own; only changes INSIDE the copy
# are the offence, so ordinary work in a repository that happens to hold one is untouched.
# =======================================================================================
foreign() {
    local repo="${1:-}" base="${2:-}" ref="${3:-}" dirs d f hit="" home
    [ -n "$repo" ] && [ -n "$base" ] && [ -n "$ref" ] || {
        echo "skew: foreign needs <repo> <base> <ref>" >&2; return 1; }

    # THE OVERRIDE IS HONOURED HERE, in the fence itself, not in the one caller. A fence is a
    # polite refusal and not a wall, and an override that only works from whichever program
    # happened to invoke it is an override nobody finds when they need it.
    [ -n "${SPIRA_ALLOW_FOREIGN_HARNESS:-}" ] && return 0

    # The harness's own repository is exempt, and that is the whole point of the rule rather
    # than an exception to it: this fence exists to send harness work THERE.
    # IN SPLIT-CHECKOUT MODE, SPIRA_REPO is the production checkout (derived from where
    # conf.sh sits) while the repo-map's home entry is the development checkout where
    # branches actually land. Both are the harness's own; check both so that a branch in the
    # dev checkout is not rejected as a foreign copy when the gate runs from prod.
    if spira_same_repo "$repo" "$SPIRA_REPO"; then return 0; fi
    local _skew_home_root
    _skew_home_root="$(repo_root "$(spira_home_repo)" 2>/dev/null)" || true
    if [ -n "$_skew_home_root" ] && spira_same_repo "$repo" "$_skew_home_root"; then return 0; fi

    # In split-checkout mode, repo_root for the home repo resolves through SPIRA_REPO to the
    # PRODUCTION checkout — not the development source where work actually lands. The check
    # above already covers that case. What is NOT covered: the dev source itself, whose path
    # lives in the repo map's `path` field. REPO_FIELD, NOT REPO_ROOT: repo_root returns the
    # production copy path in split-checkout mode, so the check above and a repo_root call
    # here are identical and the split-checkout case is missed. repo_field reads the raw path
    # from the map entry, bypassing the SPIRA_REPO lookup. That is how the dev source is
    # exempted when the gate runs from the prod copy.
    local _home_root
    _home_root="$(repo_field "$(spira_home_repo)" path 2>/dev/null)" || true
    if [ -n "$_home_root" ] && spira_same_repo "$repo" "$_home_root"; then return 0; fi

    dirs="$(harness_in_ref "$repo" "$ref")"
    [ -n "$dirs" ] || return 0          # no copy in that ref; nothing this fence judges

    while IFS= read -r f; do
        [ -n "$f" ] || continue
        while IFS= read -r d; do
            [ -n "$d" ] || continue
            case "$d" in
                .) hit="$hit$f"$'\n' ;;
                *) case "$f/" in "$d"/*) hit="$hit$f"$'\n' ;; esac ;;
            esac
        done <<< "$dirs"
    done < <(git -C "$repo" diff --name-only "$base...$ref" 2>/dev/null)

    [ -n "$hit" ] || return 0
    printf '%s' "$hit" | sort -u
    home="$(spira_home_repo)"
    {
        echo
        echo "REFUSED by skew.sh — this branch changes a COPY of the harness."
        echo
        printf '%s' "$hit" | sort -u | sed 's/^/    /'
        echo
        echo "Those paths are inside a vendored copy of the harness, in a repository that is"
        echo "not the harness's own. The harness in force is ${SPIRA_REPO}, so work landing"
        echo "here would pass its gate, close its bead, and never run. Nothing downstream can"
        echo "tell the difference: the tree that was edited is self-consistent."
        echo
        echo "Move the change to repo:${home} and re-cut the branch there. If the vendored copy"
        echo "is what you actually meant to change, say so:  SPIRA_ALLOW_FOREIGN_HARNESS=1"
    } >&2
    return 1
}

# =======================================================================================
# copies — every mapped repository that carries a harness, and whether it is ours.
#
# One line per copy: `<repo-name> <path> <harness-dir> self|second`. A repository the map
# names but this box does not have is skipped in silence, because a map is shared across
# boxes and a row for another machine is not a fault here.
# =======================================================================================
copies() {
    local n p d found=0
    for n in $(repo_names); do
        p="$(repo_root "$n" 2>/dev/null)" || continue
        [ -n "$p" ] && [ -e "$p/.git" ] || continue
        while IFS= read -r d; do
            [ -n "$d" ] || continue
            found=1
            if spira_same_repo "$p" "$SPIRA_REPO"; then
                printf '%s %s %s self\n' "$n" "$p" "$d"
            else
                printf '%s %s %s second\n' "$n" "$p" "$d"
            fi
        done < <(harness_in "$p")
    done
    [ "$found" = 1 ]
}


# =======================================================================================
# check — is the activated release the latest published, and does MANIFEST match its tag?
#
# Read-only by default: reports to stdout and exits 0/1/3 but does NOT file an ask.
# Escalation is behind --escalate, which only the timer unit passes.
# =======================================================================================
check() {
    local do_escalate=0
    while [ $# -gt 0 ]; do
        case "$1" in
            --escalate) do_escalate=1; shift ;;
            *) echo "skew: unknown flag: $1" >&2; return 1 ;;
        esac
    done

    if [ -z "${SPIRA_RELEASES:-}" ]; then
        echo "skew: SPIRA_RELEASES is not set — cannot check release currency" >&2
        return 3
    fi

    local current_link="$SPIRA_RELEASES/current"
    if [ ! -L "$current_link" ]; then
        echo "skew: no release is activated at $current_link" >&2
        return 3
    fi

    local activated_name
    activated_name="$(readlink "$current_link" 2>/dev/null)" || {
        echo "skew: cannot read symlink $current_link" >&2; return 3; }

    local manifest="$SPIRA_RELEASES/$activated_name/MANIFEST"
    if [ ! -f "$manifest" ]; then
        echo "skew: MANIFEST missing at $manifest" >&2
        return 3
    fi

    local manifest_commit
    manifest_commit="$(grep '^commit ' "$manifest" | head -1 | awk '{print $2}')"
    if [ -z "$manifest_commit" ] || ! printf '%s' "$manifest_commit" | grep -qE '^[0-9a-f]{40}$'; then
        echo "skew: MANIFEST at $manifest has no valid commit SHA" >&2
        return 3
    fi

    # THE POSITIVE CONTROL: at least one release tag must exist before the check can claim
    # anything is current. No tags means the check cannot prove currency or detect a mismatch.
    local all_tags
    all_tags="$(git -C "$SPIRA_REPO" tag -l 'spira-release-*' 2>/dev/null | sort)"
    if [ -z "$all_tags" ]; then
        echo "skew: no release tags found in $SPIRA_REPO — cannot determine release currency" >&2
        return 3
    fi

    local latest_tag findings="" hard=0 cond_not_latest=0 cond_mismatch=0
    latest_tag="$(printf '%s\n' "$all_tags" | tail -1)"

    # Read the .tag sidecar written by deploy.sh. It maps the release directory to the
    # release tag without relying on timestamps matching between the tarball and the tag.
    local release_tag=""
    local tag_sidecar="$SPIRA_RELEASES/$activated_name/.tag"
    if [ -f "$tag_sidecar" ]; then
        release_tag="$(tr -d '\n' < "$tag_sidecar" 2>/dev/null)"
    fi

    # ---------------------------------------------------------------- NOT-LATEST
    # Compare by tag when the sidecar is present; fall back to timestamp suffix otherwise.
    local activated_ts="${activated_name#spira-}"
    if [ -n "$release_tag" ]; then
        if [ "$release_tag" != "$latest_tag" ]; then
            hard=1; cond_not_latest=1
            findings="${findings}NOT-LATEST activated $activated_name is not the latest published release $latest_tag
"
        fi
    else
        local latest_ts="${latest_tag##*-}"
        if [ "$activated_ts" != "$latest_ts" ]; then
            hard=1; cond_not_latest=1
            findings="${findings}NOT-LATEST activated $activated_name is not the latest published release $latest_tag
"
        fi
    fi

    # ---------------------------------------------------------------- MANIFEST-MISMATCH
    # Use the sidecar tag when available; fall back to timestamp suffix matching.
    if [ -z "$release_tag" ]; then
        while IFS= read -r t; do
            case "$t" in *"-${activated_ts}") release_tag="$t"; break ;; esac
        done <<< "$all_tags"
    fi

    if [ -n "$release_tag" ]; then
        local tag_commit
        tag_commit="$(git -C "$SPIRA_REPO" rev-parse "${release_tag}^{commit}" 2>/dev/null)" || tag_commit=""
        if [ -n "$tag_commit" ] && [ "$manifest_commit" != "$tag_commit" ]; then
            hard=1; cond_mismatch=1
            findings="${findings}MANIFEST-MISMATCH MANIFEST records $manifest_commit but release tag $release_tag points at $tag_commit
"
        fi
    else
        findings="${findings}CANNOT-VERIFY no release tag found for $activated_name; MANIFEST commit $manifest_commit unverified
"
    fi

    if [ -z "$findings" ]; then
        printf 'skew: in effect — %s is the latest published release; MANIFEST matches tag\n' \
            "$activated_name"
        return 0
    fi

    printf '%s' "$findings"

    # A CANNOT- line alone is not a verdict in either direction; exit 3, not 1.
    if [ "$hard" = 0 ]; then
        echo "skew: the check could not complete — this is not a clean verdict" >&2
        return 3
    fi

    if [ "$do_escalate" = 1 ]; then
        local condition_key="v2:NOT-LATEST=${cond_not_latest} MANIFEST-MISMATCH=${cond_mismatch}"
        escalate "$condition_key" "$findings"
    fi
    return 1
}

# =======================================================================================
# escalate — once per distinct CONDITION, not once per pass.
#
# Keyed on which finding TYPES are present (BEHIND/DIRTY/COPY/STALE yes/no), not on the
# findings text. The text carries measurements — commits behind, file list — that drift every
# pass while the condition stays constant, which produced a new ask every hour as the repo
# fell further behind (sp-624f). The condition key is versioned (v2:) so a stamp written by
# an older scheme cannot match and causes one re-escalation on upgrade.
#
# Each condition key is a stable, versioned identifier. BEHIND=1 and DIRTY=1 are distinct
# keys, so a mail about one condition does not suppress the other.
# =======================================================================================
escalate() {
    local condition_key="$1" findings="$2"

    # Check whether this condition has already been escalated in this run. The condition
    # key is versioned and stable, so a repeated call with the same key in the same
    # SPIRA_RUN should not fire again — within one run we escalate once.
    local escalate_stamp="$SPIRA_RUN/skew.escalated-$(printf '%s' "$condition_key" | cksum | cut -d' ' -f1)"
    if [ -f "$escalate_stamp" ]; then
        echo "skew: condition already reported — $(cat "$escalate_stamp" 2>/dev/null || echo "$condition_key")"
        return 0
    fi

    # Stdout goes to skew.log under the service unit. Stderr does too (both streams are
    # captured), but everything below writes to stdout so the delivery path is explicit and
    # does not depend on StandardError being redirected — which has changed once already.
    if [ ! -x "$SPIRA_HOME/mail.sh" ]; then
        echo "skew: mail.sh not found — the finding above reaches nobody"
        return 1
    fi

    local default_action
    if [[ "$condition_key" == *"MANIFEST-MISMATCH=1"* ]]; then
        default_action="the release artifact and its git tag disagree — verify the release was built from the correct commit; rebuild and re-activate if not"
    else
        default_action="activate the latest published release — download the latest tarball and run activate.sh with it"
    fi
    local _subj="The Spira copy in force is not the code that landed"
    local notify_out notify_rc
    notify_out="$("$SPIRA_HOME/mail.sh" send operator \
        --from "Skew check <skew@spira>" \
        --subject "$_subj" \
        --kind question \
        --default "$default_action" <<MAILEOF 2>&1)"; notify_rc=$?
## Question
$_subj

## Default
$default_action

beads can be closed, gated and merged while the behaviour they changed never takes effect — the tree that was edited is self-consistent, so nothing downstream reports a fault

$findings
MAILEOF

    if [ "$notify_rc" != 0 ]; then
        echo "skew: escalation failed (rc=$notify_rc): $notify_out"
        return "$notify_rc"
    fi
    # Write the stamp so subsequent calls in this run do not re-escalate the same condition.
    mkdir -p "$SPIRA_RUN" 2>/dev/null || true
    printf '%s' "$condition_key" > "$escalate_stamp" 2>/dev/null || true
    echo "skew: escalated — $condition_key"
}

# =======================================================================================
# refresh — fast-forward a checkout to its base ref.
#
# Called unconditionally by the landing pass, so a base ref that moved by ANY route — a
# branch merged, a push from another box, a PR merged on GitHub, a hand-landing — is picked
# up within one pass rather than waiting for `check` to escalate it an hour later.
#
# THE GUARDS ARE THE WHOLE POINT. ff-only, clean-tracked-tree, and on-the-base-branch: any
# violation means something a mechanical advance must not override. A declined refresh names
# which condition refused it, because a refresh that stopped happening is indistinguishable
# in the log from one with nothing to do — the silence that cost eight hours on landing.sh.
#
# TRACKED FILES ONLY. An operator's untracked notes beside the code are their own business;
# a MODIFIED tracked file is code in force that is on no branch. This is the same check as
# `check` above, and the two must agree — --untracked-files=no in both.
# =======================================================================================
refresh() {
    local repo="${1:-$SPIRA_REPO}" base base_branch remote behind dirty current
    # In release mode the checkout is not what systemd executes; fast-forwarding it would
    # advance code that nothing runs and is not the release that is in force.
    if [ -n "${SPIRA_RELEASES:-}" ] && [ -L "$SPIRA_RELEASES/current" ]; then
        echo "skew: refresh skipped — release mode is active ($SPIRA_RELEASES/current)"
        return 0
    fi
    [ -e "$repo/.git" ] || {
        echo "skew: refresh: $repo is not a git checkout"; return 1; }
    base="$(spira_landref "$repo")" || {
        echo "skew: refresh: cannot resolve the ref $repo lands on"; return 1; }
    base_branch="$(ref_branch "$base")"
    remote="$(ref_remote "$base" 2>/dev/null)" || remote=""
    [ -n "$remote" ] && git -C "$repo" fetch -q --no-write-fetch-head "$remote" 2>/dev/null

    behind="$(git -C "$repo" rev-list --count "HEAD..$base" 2>/dev/null || echo 0)"
    [ "${behind:-0}" -gt 0 ] || return 0

    dirty="$(git -C "$repo" status --porcelain --untracked-files=no 2>/dev/null)"
    if [ -n "$dirty" ]; then
        # Write a stamp so the decline is countable without reading the log — watchtower
        # and check() can read it without tailing skew.log (law-detection-outranks-rejection).
        mkdir -p "$SPIRA_RUN" 2>/dev/null || true
        { printf 'tracked files are modified\n'; printf '%s\n' "$dirty"; } \
            > "$SPIRA_RUN/skew.refresh-declined" 2>/dev/null || true
        echo "skew: refresh declined — tracked files are modified"; return 1; fi

    current="$(git -C "$repo" branch --show-current 2>/dev/null)"
    if [ "$current" != "$base_branch" ]; then
        echo "skew: refresh declined — checkout is on ${current:-a detached HEAD}, not $base_branch"; return 1; fi

    if ! git -C "$repo" merge --ff-only -q "$base" 2>/dev/null; then
        echo "skew: refresh declined — cannot fast-forward to $base"; return 1; fi

    # Clear the dirty-decline stamp on a successful refresh so watchers see the recovery.
    rm -f "$SPIRA_RUN/skew.refresh-declined" 2>/dev/null || true
    echo "skew: refreshed to $base ($behind commit(s))"
    return 0
}

# =======================================================================================
# units — are the installed units what the templates render?
#
# EXIT   0  the installed units match
#        1  at least one differs — the output names which
#        3  the check itself could not run (install.sh missing, render failure)
#
# This is the standalone entry point; `check` includes it in its audit. It costs no database
# call: install.sh --diff renders templates from conf.sh and diffs files on disk.
# =======================================================================================
units() {
    local installer stale_out stale_rc
    installer="$(cd "$SPIRA_HOME/../systemd" 2>/dev/null && pwd -P)/install.sh"
    if [ ! -r "$installer" ]; then
        printf 'skew: install.sh is missing at %s — unit staleness has no answer\n' "$installer" >&2
        return 3
    fi
    stale_out="$(bash "$installer" --diff 2>&1)"; stale_rc=$?
    if [ "$stale_rc" = 0 ]; then
        printf 'skew: units — installed units match what this box renders\n'
        return 0
    fi
    printf '%s\n' "$stale_out"
    return 1
}

case "${1:-check}" in
    check)   [ "${1:-}" = check ] && shift; check "$@" ;;
    units)   units ;;
    refresh) shift; refresh "$@" ;;
    copies)  copies || { echo "skew: no mapped repository carries a harness — the map or the matcher is wrong" >&2; exit 3; } ;;
    foreign) shift; foreign "$@" ;;
    *)       sed -n '3,9p' "$0" >&2; exit 2 ;;
esac
