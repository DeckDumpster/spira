#!/usr/bin/env bash
#
# skew.sh — is the ACTIVATED release the latest published one?
#
#   skew.sh check [--escalate]             audit this box; escalate on findings (only with --escalate)
#   skew.sh units                          are the installed units what the templates render?
#   skew.sh refresh [repo]                 fast-forward the checkout to its base ref
#                                           (queue.local: check only — alarms on a mismatch
#                                           between what is running and local/main's head,
#                                           deploys nothing; queue land-local deploys)
#   skew.sh gap [repo]                     report commits between HEAD and the base ref
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
# `check` REPORTS; it does not repair. Activation is release install-tarball's job.
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


# harness_in <repo-path> -> the harness directories in its WORKING TREE, one per line.
# Exit 1 when it holds none, which is the ordinary answer for most repositories.
harness_in() {
    local root="$1"
    [ -e "$root/.git" ] || return 1
    git -C "$root" ls-files 2>/dev/null | exclude.sh harness-in 2>/dev/null
}

# harness_in_ref <repo-path> <ref> -> the harness directories in that REF, one per line.
# The ref and not the checkout: the question a landing gate asks is what the repository would
# contain after this branch merges, and the branch may be the thing that adds the copy.
harness_in_ref() {
    local root="$1" ref="$2"
    git -C "$root" ls-tree -r --name-only "$ref" 2>/dev/null | exclude.sh harness-in 2>/dev/null
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
        # CHECKOUT MODE: a git checkout with no activated release answers via gap — is HEAD
        # at the base ref? This is the question the service exists to answer on a box that
        # runs directly from a checkout rather than from an activated release tarball.
        if [ -d "$SPIRA_REPO/.git" ] || [ -f "$SPIRA_REPO/.git" ]; then
            local _gap_out _gap_rc
            _gap_out="$(gap "$SPIRA_REPO" 2>&1)"; _gap_rc=$?
            printf '%s\n' "$_gap_out"
            if [ "$_gap_rc" = 1 ] && [ "$do_escalate" = 1 ]; then
                escalate "v2:BEHIND=1" "BEHIND ${_gap_out}"
            fi
            return "$_gap_rc"
        fi
        echo "skew: no release is activated at $current_link — cannot determine skew" >&2
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

    # LOCAL RELEASE MODE (queue.local): nothing landing through this box's own home repo
    # ever gets a release tag — `queue land-local` builds, verifies and activates a commit
    # straight off local/main, with no push to a forge. The tag machinery below (all_tags,
    # release_tag, NOT-LATEST/MANIFEST-MISMATCH) answers a question this repo never has an
    # answer to, so it never runs for it. A repository that still lands through push/pr (the
    # public pipeline) is untouched: repo_land defaults to "push", so this never fires there.
    if [ "$(repo_land "$(spira_home_repo)")" = "queue.local" ]; then
        check_local "$activated_name" "$manifest_commit" "$do_escalate"
        return $?
    fi

    # THE POSITIVE CONTROL: at least one release tag must exist before the check can claim
    # anything is current. No tags means the check cannot prove currency or detect a mismatch.
    # Artifact mode: SPIRA_REPO has no .git; fall back to gh release list when
    # SPIRA_RELEASE_REPO is set. Defaults to SPIRA_GH_INTAKE_REPO for single-repo installs.
    # SPIRA_RELEASE_REPO names the source: a forge repository (owner/repo, read with gh), or a
    # LOCAL directory (a path or file://) of release tarballs, each beside a <stem>.tag sidecar
    # naming its tag — the source for a box whose user units have no forge credential.
    local all_tags=""
    all_tags="$(git -C "$SPIRA_REPO" tag -l 'spira-release-*' 2>/dev/null | sort)"
    local _rel_repo="${SPIRA_RELEASE_REPO:-${SPIRA_GH_INTAKE_REPO:-}}"
    local _rel_dir=""
    case "$_rel_repo" in
        file://*) _rel_dir="${_rel_repo#file://}" ;;
        /*)       _rel_dir="$_rel_repo" ;;
    esac
    if [ -z "$all_tags" ] && [ -n "$_rel_dir" ]; then
        if [ ! -d "$_rel_dir" ]; then
            echo "skew: SPIRA_RELEASE_REPO names the local release directory $_rel_dir, which does not exist" >&2
            return 3
        fi
        all_tags="$(cat "$_rel_dir"/*.tag 2>/dev/null | tr -d ' \t' | grep '^spira-release-' | sort -u)"
    elif [ -z "$all_tags" ] && [ -n "$_rel_repo" ]; then
        local _rel_json="" _rel_err="$SPIRA_RUN/.skew-gh-err.$$"
        mkdir -p "$SPIRA_RUN" 2>/dev/null || true
        if ! _rel_json="$(ghq release list --repo "$_rel_repo" --json tagName,isDraft 2>"$_rel_err")"; then
            printf 'skew: could not list the releases of %s with gh (%s) — make a gh credential visible to user units (the systemd user manager'"'"'s environment, or gh auth as %s), or set SPIRA_RELEASE_REPO to a local directory of release tarballs\n' \
                "$_rel_repo" "$(head -1 "$_rel_err" 2>/dev/null)" "$(id -un 2>/dev/null)" >&2
            rm -f "$_rel_err"
            return 3
        fi
        rm -f "$_rel_err"
        if [ -n "$_rel_json" ]; then
            all_tags="$(printf '%s' "$_rel_json" | python3 -c '
import json, sys
try:
    releases = json.load(sys.stdin)
    pub = [r["tagName"] for r in releases
           if not r.get("isDraft", True)
           and r["tagName"].startswith("spira-release-")]
    pub.sort()
    for t in pub: print(t)
except Exception:
    pass
' 2>/dev/null)"
        fi
    elif [ -z "$all_tags" ]; then
        echo "skew: no release source — set SPIRA_RELEASE_REPO to the forge repository that publishes this install's releases (owner/repo) or to a local directory of release tarballs" >&2
        return 3
    fi
    if [ -z "$all_tags" ]; then
        echo "skew: no release tags found in ${_rel_dir:-${_rel_repo:-the checkout}} — cannot determine release currency" >&2
        return 3
    fi

    local latest_tag findings="" hard=0 cond_not_latest=0 cond_mismatch=0
    latest_tag="$(printf '%s\n' "$all_tags" | tail -1)"

    # Read the .tag sidecar. Sidecar lives in .tags/ beside the release dirs, not inside
    # the release dir (which is read-only after release install-tarball runs chmod -R a-w).
    local release_tag=""
    local tag_sidecar="$SPIRA_RELEASES/.tags/$activated_name"
    if [ -f "$tag_sidecar" ]; then
        release_tag="$(tr -d '\n' < "$tag_sidecar" 2>/dev/null)"
    fi

    # No sidecar: find the release tag whose commit matches the MANIFEST commit.
    # Never fall back to timestamps — tarball and tag stamps diverge legitimately (issue #51).
    if [ -z "$release_tag" ]; then
        local _t _tc
        while IFS= read -r _t; do
            _tc="$(git -C "$SPIRA_REPO" rev-parse "${_t}^{commit}" 2>/dev/null)" || continue
            [ "$_tc" = "$manifest_commit" ] && { release_tag="$_t"; break; }
        done <<< "$all_tags"
        unset _t _tc
    fi

    # ---------------------------------------------------------------- NOT-LATEST
    if [ -n "$release_tag" ] && [ "$release_tag" != "$latest_tag" ]; then
        hard=1; cond_not_latest=1
        findings="${findings}NOT-LATEST activated $activated_name is not the latest published release $latest_tag
"
    fi

    # ---------------------------------------------------------------- MANIFEST-MISMATCH
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
# check_local — the queue.local answer to "is the activated release current". No release
# tag ever exists for a commit this box landed itself (nothing pushes one), so `check`'s tag
# machinery cannot answer the question at all here; this is the branch that runs instead.
#
# `release verify` and `release status` do the deterministic work (MANIFEST/hash checking,
# the hotfix record); this glues their answers to the one question skew asks. Sourced with
# lib.sh, `release` is the release crate's own binary, resolved off PATH like every other
# release-tier program this file already calls (e.g. `release build/verify/activate` in
# refresh(), below).
#
#   MATCH     MANIFEST commit == the home repo's landed ref tip                -> clean
#   HOTFIX    MANIFEST commit is not the tip, but `release status` records it
#             as the standing hotfix                                          -> clean
#   BEHIND    MANIFEST commit is an ancestor of the tip, no hotfix recorded    -> finding
#   TAMPERED  `release verify` finds the release does not match its own MANIFEST -> finding
#   otherwise (commit unresolvable against the tip, no hotfix explains it)    -> CANNOT-VERIFY
#
# Fail closed (law-absence-needs-a-positive-control): an unexplained divergence is
# CANNOT-VERIFY, never silently accepted as current.
# =======================================================================================
check_local() {
    local activated_name="$1" manifest_commit="$2" do_escalate="$3"
    local home repo base tip
    home="$(spira_home_repo)"
    repo="$(repo_root "$home" 2>/dev/null)"
    if [ -z "$repo" ] || [ ! -e "$repo/.git" ]; then
        echo "skew: cannot check — repo:$home has no resolvable git checkout" >&2
        return 3
    fi
    base="$(spira_landref "$home" 2>/dev/null)" || {
        echo "skew: cannot check — cannot resolve the ref repo:$home lands on" >&2; return 3; }
    tip="$(git -C "$repo" rev-parse -q --verify "$base" 2>/dev/null)" || {
        echo "skew: cannot check — ref $base does not resolve in $repo" >&2; return 3; }

    local findings="" hard=0 cond_behind=0 cond_tampered=0 hotfix_line=""

    # ---------------------------------------------------------------- LOCAL-TAMPERED
    # --no-pre-activate: this is a drift check, not an activation rehearsal — it must never
    # be Dolt/bead-store bound (the design's own rule), so it skips pre-activate's store
    # check and asks only whether the release still matches its own MANIFEST.
    local verify_out
    if ! verify_out="$(release verify "$activated_name" --no-pre-activate 2>&1)"; then
        hard=1; cond_tampered=1
        findings="${findings}LOCAL-TAMPERED release $activated_name fails verify: $(printf '%s' "$verify_out" | tr '\n' ' ')
"
    fi

    # ---------------------------------------------------------------- LOCAL-BEHIND / hotfix
    if [ "$manifest_commit" != "$tip" ]; then
        hotfix_line="$(release status 2>/dev/null | grep '^RUNNING UNLANDED ' || true)"
        case "$hotfix_line" in
            "RUNNING UNLANDED $manifest_commit:"*) ;;  # a recorded, deliberate divergence
            *)
                if git -C "$repo" merge-base --is-ancestor "$manifest_commit" "$tip" 2>/dev/null; then
                    local behind
                    behind="$(git -C "$repo" rev-list --count "$manifest_commit..$tip" 2>/dev/null)"
                    hard=1; cond_behind=1
                    findings="${findings}LOCAL-BEHIND activated $activated_name ($manifest_commit) is ${behind:-an unresolved number of} commit(s) behind $base ($tip)
"
                else
                    findings="${findings}CANNOT-VERIFY activated $activated_name ($manifest_commit) is neither $base's tip ($tip) nor a recorded standing hotfix
"
                fi
                ;;
        esac
    fi

    if [ -z "$findings" ]; then
        if [ "$manifest_commit" = "$tip" ]; then
            printf 'skew: in effect — %s matches %s (%s); MANIFEST verifies\n' "$activated_name" "$base" "$tip"
        else
            printf 'skew: in effect — %s is a recorded standing hotfix (%s); %s is at %s; MANIFEST verifies\n' \
                "$activated_name" "$hotfix_line" "$base" "$tip"
        fi
        return 0
    fi

    printf '%s' "$findings"

    # A CANNOT-VERIFY line alone is not a verdict in either direction; exit 3, not 1.
    if [ "$hard" = 0 ]; then
        echo "skew: the check could not complete — this is not a clean verdict" >&2
        return 3
    fi

    if [ "$do_escalate" = 1 ]; then
        local condition_key="v2:LOCAL-BEHIND=${cond_behind} LOCAL-TAMPERED=${cond_tampered}"
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

    local default_action
    if [[ "$condition_key" == *"MANIFEST-MISMATCH=1"* ]]; then
        default_action="the release artifact and its git tag disagree — verify the release was built from the correct commit; rebuild and re-activate if not"
    elif [[ "$condition_key" == *"LOCAL-TAMPERED=1"* ]]; then
        default_action="the activated release no longer matches its own MANIFEST — release build the commit again and release activate the rebuilt release"
    elif [[ "$condition_key" == *"LOCAL-BEHIND=1"* ]]; then
        default_action="the activated release is behind local/main's tip — land it forward with queue land-local, or if the tip is bad, queue rollback-local"
    elif [[ "$condition_key" == *"LOCAL-SKEW=1"* ]]; then
        default_action="what is running does not match local/main's head — land the round again with queue land-local, or undo with queue rollback-local; refresh will not act on its own"
    else
        default_action="activate the latest published release — download the latest tarball and run release install-tarball with it"
    fi
    local _subj="The Spira copy in force is not the code that landed"
    local _body
    _body="$(cat <<MAILEOF
## Question
$_subj

## Default
$default_action

beads can be closed, gated and merged while the behaviour they changed never takes effect — the tree that was edited is self-consistent, so nothing downstream reports a fault

$findings
MAILEOF
)"
    local notify_out notify_rc
    notify_out="$(printf '%s\n' "$_body" | mail send operator \
        --from "Skew check <skew@spira>" \
        --subject "$_subj" \
        --kind question \
        --default "$default_action" 2>&1)"; notify_rc=$?

    if [ "$notify_rc" != 0 ]; then
        echo "skew: escalation failed (rc=$notify_rc): $notify_out"
        return "$notify_rc"
    fi
    # Write the stamp so subsequent calls in this run do not re-escalate the same condition.
    mkdir -p "$SPIRA_RUN" 2>/dev/null || true
    printf '%s' "$condition_key" > "$escalate_stamp" 2>/dev/null || true
    echo "skew: escalated — $condition_key"
}

# _skew_repo_name_for_path <path> -> the repo-map name whose root is that path, by object
# store (spira_same_repo), not string comparison — the same reason copies() above resolves
# every mapped name rather than testing the path directly.
_skew_repo_name_for_path() {
    local path="$1" n p
    for n in $(repo_names); do
        p="$(repo_root "$n" 2>/dev/null)" || continue
        [ -n "$p" ] || continue
        if spira_same_repo "$path" "$p" 2>/dev/null; then printf '%s' "$n"; return 0; fi
    done
    return 1
}

# _refresh_check_only <repo> <name> -> queue.local's refresh: compare what is running (the
# activated release's MANIFEST commit, or HEAD in a plain checkout) against local/main's
# head. Equal: nothing to report, exit 0. Unequal: print the finding, escalate once per
# distinct running/expected pair, exit 1 — and, either way, NEVER touch the checkout, build
# anything, or swing the current symlink. That is queue land-local's job alone.
_refresh_check_only() {
    local repo="$1" name="$2" base base_sha running
    # BY NAME, NOT BY PATH. spira_landref given a path re-derives the name via
    # repo_name_at, which prefers SPIRA_HOME_REPO for a path matching SPIRA_REPO — the
    # caller already found the one true name for this path (_skew_repo_name_for_path,
    # by object store); re-deriving it a second, weaker way here would only be able to
    # get it wrong.
    base="$(spira_landref "$name" 2>/dev/null)" || {
        echo "skew: refresh: queue.local — cannot resolve the ref $repo lands on"; return 1; }
    base_sha="$(git -C "$repo" rev-parse -q --verify "$base" 2>/dev/null)" || {
        echo "skew: refresh: queue.local — ref $base does not resolve"; return 1; }

    if [ -n "${SPIRA_RELEASES:-}" ] && [ -L "$SPIRA_RELEASES/current" ]; then
        local activated_name manifest
        activated_name="$(readlink "$SPIRA_RELEASES/current" 2>/dev/null)"
        manifest="$SPIRA_RELEASES/$activated_name/MANIFEST"
        running="$(grep '^commit ' "$manifest" 2>/dev/null | head -1 | awk '{print $2}')"
        [ -n "$running" ] || {
            echo "skew: refresh: queue.local — MANIFEST missing or unreadable at $manifest"; return 1; }
    else
        [ -e "$repo/.git" ] || {
            echo "skew: refresh: $repo is not a git checkout"; return 1; }
        running="$(git -C "$repo" rev-parse -q --verify HEAD 2>/dev/null)"
    fi

    if [ "$running" = "$base_sha" ]; then
        echo "skew: refresh: queue.local — running ($running) matches $base; nothing to deploy"
        return 0
    fi

    # A recorded standing hotfix (release status) explains the mismatch on purpose — it is
    # not skew, it is the operator's own stop-the-world fix, already visible to doctor.sh and
    # watchtower through the same record. Never alarm on the condition its own mechanism
    # exists to allow.
    local hotfix_line
    hotfix_line="$(release status 2>/dev/null | grep '^RUNNING UNLANDED ' || true)"
    case "$hotfix_line" in
        "RUNNING UNLANDED $running:"*)
            echo "skew: refresh: queue.local — running ($running) is a recorded standing hotfix ($hotfix_line); refresh never resets it"
            return 0
            ;;
    esac

    local finding
    finding="LOCAL-SKEW running $running does not match $base ($base_sha) — queue.local deploys only through queue land-local; refresh never resets it"
    echo "skew: $finding"
    escalate "v1:LOCAL-SKEW=1" "$finding"
    return 1
}

# =======================================================================================
# refresh — advance a checkout to its base ref via stage-and-swap.
#
# Called unconditionally by the landing pass, so a base ref that moved by ANY route — a
# branch merged, a push from another box, a PR merged on GitHub, a hand-landing — is picked
# up within one pass rather than waiting for `check` to escalate it an hour later.
#
# STAGE-AND-SWAP, NOT ff-only. Each changed file is written beside the existing one and
# atomically renamed into place (write + mv). A running aeon that opened the file by path
# keeps its old inode; new invocations open the new inode. This lets refresh advance even
# while aeons are active (law-replace-running-scripts-atomically).
#
# ON-THE-BASE-BRANCH is still a hard guard — a detached HEAD or a feature branch means
# something else is controlling the checkout. A declined refresh names the condition because
# a refresh that stops is indistinguishable in the log from one with nothing to do.
#
# QUEUE.LOCAL NEVER REACHES EITHER BRANCH BELOW. Under queue.local, queue land-local is
# THE ONLY DEPLOYER — it publishes and activates the round's own binaries as a release. A
# refresh that instead reset the checkout (stage-and-swap) or rebuilt from source (release
# mode's `release build`) would deploy something no round ever certified, so this repo gets a
# third path: compare what is running against local/main's head and alarm on a mismatch,
# never act on one.
# =======================================================================================
refresh() {
    local repo="${1:-}"
    if [ -z "$repo" ]; then
        # NO EXPLICIT REPO: the periodic timer's own call (its ExecStartPre passes none).
        # $SPIRA_REPO, once a release is activated, names the RELEASE directory the running
        # tree was built into (conf.sh derives it from $SPIRA_HOME/.., under spira-releases/
        # — never a git checkout on its own), not the harness's own working checkout. Prefer
        # repo_root's answer for the home repo (the same repo-map lookup foreign() and check()
        # already use); fall back to $SPIRA_REPO exactly as before when that does not resolve
        # to a checkout, so every other caller's behaviour is unchanged.
        repo="$(repo_root "$(spira_home_repo)" 2>/dev/null)"
        [ -n "$repo" ] && [ -e "$repo/.git" ] || repo="$SPIRA_REPO"
    fi
    local base base_branch remote behind dirty current
    local _qlref_name; _qlref_name="$(_skew_repo_name_for_path "$repo" 2>/dev/null)" || _qlref_name=""
    if [ -n "$_qlref_name" ] && [ "$(repo_land "$_qlref_name" 2>/dev/null)" = "queue.local" ]; then
        _refresh_check_only "$repo" "$_qlref_name"
        return $?
    fi
    if [ -n "${SPIRA_RELEASES:-}" ] && [ -L "$SPIRA_RELEASES/current" ]; then
        # Release mode: fetch the base branch, build a new release, flip current.
        # The git checkout is not the running tree; no stage-and-swap needed.
        [ -e "$repo/.git" ] || {
            echo "skew: refresh: $repo is not a git checkout"; return 1; }
        local _base _remote _behind
        _base="$(spira_landref "$repo")" || {
            echo "skew: refresh: cannot resolve the ref $repo lands on"; return 1; }
        _remote="$(ref_remote "$_base" "$repo" 2>/dev/null)" || _remote=""
        [ -n "$_remote" ] && git -C "$repo" fetch -q --no-write-fetch-head "$_remote" 2>/dev/null
        _behind="$(git -C "$repo" rev-list --count "HEAD..$_base" 2>/dev/null || echo 0)"
        if [ "${_behind:-0}" -eq 0 ]; then
            echo "skew: refresh: release mode — already at $_base"
            return 0
        fi
        git -C "$repo" merge --ff-only -q "$_base" 2>/dev/null || {
            echo "skew: refresh: cannot fast-forward to $_base"; return 1; }
        local _sha
        _sha="$(git -C "$repo" rev-parse HEAD)" \
            && release build "$_sha" --repo "$repo" --releases "$SPIRA_RELEASES" >/dev/null \
            && release verify "$_sha" --releases "$SPIRA_RELEASES" \
            && release activate "$_sha" --repo "$repo" --landed-ref "$_base" --releases "$SPIRA_RELEASES" || {
            echo "skew: refresh: release build/verify/activate of $_sha failed"; return 1; }
        echo "skew: refreshed — new release installed ($_behind commit(s))"
        overrides.sh apply "$repo"
        return 0
    fi
    [ -e "$repo/.git" ] || {
        echo "skew: refresh: $repo is not a git checkout"; return 1; }
    base="$(spira_landref "$repo")" || {
        echo "skew: refresh: cannot resolve the ref $repo lands on"; return 1; }
    base_branch="$(ref_branch "$base")"
    remote="$(ref_remote "$base" "$repo" 2>/dev/null)" || remote=""
    [ -n "$remote" ] && git -C "$repo" fetch -q --no-write-fetch-head "$remote" 2>/dev/null

    behind="$(git -C "$repo" rev-list --count "HEAD..$base" 2>/dev/null || echo 0)"
    [ "${behind:-0}" -gt 0 ] || return 0

    dirty="$(git -C "$repo" status --porcelain --untracked-files=no 2>/dev/null)"
    if [ -n "$dirty" ]; then
        # Salvage rather than refuse (law-production-is-not-a-working-tree): stash the drift
        # so it is recoverable and advance anyway. Write the stamp so watchtower can see it.
        mkdir -p "$SPIRA_RUN" 2>/dev/null || true
        { printf 'tracked files are modified\n'; printf '%s\n' "$dirty"; } \
            > "$SPIRA_RUN/skew.refresh-declined" 2>/dev/null || true
        local _stash_tag="skew-refresh-$(date +%Y%m%dT%H%M%SZ)"
        local _stash_out _stash_rc
        _stash_out="$(git -C "$repo" stash push -m "$_stash_tag" 2>&1)"; _stash_rc=$?
        if [ "$_stash_rc" != 0 ]; then
            echo "skew: refresh declined — tracked files are modified and stash failed: $_stash_out"; return 1; fi
        echo "skew: refresh: stashed dirty tracked files (tag: $_stash_tag):"
        printf '%s\n' "$dirty"
    fi

    current="$(git -C "$repo" branch --show-current 2>/dev/null)"
    if [ "$current" != "$base_branch" ]; then
        echo "skew: refresh declined — checkout is on ${current:-a detached HEAD}, not $base_branch"; return 1; fi

    # Stage-and-swap each file that changed between HEAD and base.
    local _f _mode _tmp _err=0
    while IFS=$'\t' read -r _status _f; do
        [ -n "$_f" ] || continue
        case "$_status" in
            M)  _tmp="$repo/$_f.spira-new"
                if git -C "$repo" show "$base:$_f" > "$_tmp" 2>/dev/null; then
                    _mode="$(git -C "$repo" ls-tree "$base" "$_f" 2>/dev/null | awk '{print $1}')"
                    case "$_mode" in 100755) chmod 755 "$_tmp" ;; *) chmod 644 "$_tmp" ;; esac
                    mv "$_tmp" "$repo/$_f"
                else rm -f "$_tmp"; _err=$((_err+1)); fi
                ;;
            A)  mkdir -p "$repo/$(dirname "$_f")" 2>/dev/null || true
                if git -C "$repo" show "$base:$_f" > "$repo/$_f" 2>/dev/null; then
                    _mode="$(git -C "$repo" ls-tree "$base" "$_f" 2>/dev/null | awk '{print $1}')"
                    case "$_mode" in 100755) chmod 755 "$repo/$_f" ;; *) chmod 644 "$repo/$_f" ;; esac
                else _err=$((_err+1)); fi
                ;;
            D)  rm -f "$repo/$_f" ;;
        esac
    done < <(git -C "$repo" diff --diff-filter=MAD --name-status "HEAD..$base" 2>/dev/null)
    if [ "$_err" -gt 0 ]; then
        echo "skew: refresh: stage-and-swap failed for $_err file(s)"; return 1; fi

    # Update HEAD and index to reflect the new state; working tree is already current.
    git -C "$repo" reset --mixed -q "$base" 2>/dev/null || {
        echo "skew: refresh: git reset --mixed failed after stage-and-swap"; return 1; }

    # Clear the dirty-decline stamp on a successful refresh so watchers see the recovery.
    rm -f "$SPIRA_RUN/skew.refresh-declined" 2>/dev/null || true
    echo "skew: refreshed to $base ($behind commit(s))"

    # RE-APPLY DECLARED OVERRIDES IMMEDIATELY, before this function returns. The reset above
    # just wrote the base ref's own files over anything an operator override held in this
    # checkout; without this call there is a window — previously closed only by a timer
    # running once a minute outside the harness — where the reset brief is live (sp-qdh0x).
    overrides.sh apply "$repo"
    return 0
}

# =======================================================================================
# gap — how far is the running checkout behind its base ref?
#
# Exit   0  HEAD is at the base ref — 0 commits behind
#        1  HEAD is behind the base ref — the count is on stdout
#        3  cannot answer — remote unreachable, ref unresolvable, or no remote configured
#
# POSITIVE CONTROL: the ref must be resolvable after a fetch. "0 commits behind" and
# "I looked at the wrong ref" are indistinguishable — if we cannot prove the ref exists,
# we exit 3 rather than claiming currency (law-absence-needs-a-positive-control).
# =======================================================================================
gap() {
    local repo="${1:-$SPIRA_REPO}" base base_branch remote behind current base_short
    [ -e "$repo/.git" ] || { echo "skew: gap: $repo is not a git checkout" >&2; return 3; }
    base="$(spira_landref "$repo")" || {
        echo "skew: gap: cannot resolve the ref $repo lands on" >&2; return 3; }
    base_branch="$(ref_branch "$base")"
    remote="$(ref_remote "$base" "$repo" 2>/dev/null)" || remote=""
    if [ -n "$remote" ]; then
        git -C "$repo" fetch -q --no-write-fetch-head "$remote" 2>/dev/null || {
            echo "skew: gap: cannot fetch from $remote — gap is unknown" >&2; return 3; }
    fi
    git -C "$repo" rev-parse --verify -q "$base" >/dev/null 2>&1 || {
        echo "skew: gap: ref $base does not resolve — cannot report gap" >&2; return 3; }
    behind="$(git -C "$repo" rev-list --count "HEAD..$base" 2>/dev/null)" || {
        echo "skew: gap: cannot count commits between HEAD and $base" >&2; return 3; }
    current="$(git -C "$repo" rev-parse --short HEAD 2>/dev/null)"
    base_short="$(git -C "$repo" rev-parse --short "$base" 2>/dev/null)"
    if [ "${behind:-0}" = 0 ]; then
        printf 'skew: gap: HEAD (%s) is at %s — 0 commits behind\n' "$current" "$base"
        return 0
    fi
    printf 'skew: gap: HEAD (%s) is %s commit(s) behind %s (%s)\n' \
        "$current" "$behind" "$base" "$base_short"
    return 1
}

# =======================================================================================
# units — are the installed units what the templates render?
#
# EXIT   0  the installed units match
#        1  at least one differs — the output names which
#        3  the check itself could not run (install.sh missing, render failure)
#
# `check` does not call this — a released, activated tree cannot drift from its own
# templates, so unit staleness is a checkout-mode question, not a release-mode one. It costs
# no database call: install.sh --diff renders templates from conf.sh and diffs files on disk.
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
    gap)     shift; gap "$@" ;;
    copies)  copies || { echo "skew: no mapped repository carries a harness — the map or the matcher is wrong" >&2; exit 3; } ;;
    foreign) shift; foreign "$@" ;;
    *)       sed -n '3,10p' "$0" >&2; exit 2 ;;
esac
