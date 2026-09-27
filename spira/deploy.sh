#!/usr/bin/env bash
#
# deploy.sh — operator-run release deploy.
#
#   deploy.sh [--dry-run] [--force] [--allow-draft] [--tarball <path>] <tag|latest>
#
#   --tarball <path>  deploy the NAMED release from a local tarball instead of downloading it,
#                  making no forge call at all (no draft check, no asset lookup, no download).
#                  For LOCAL release acceptance only (acceptance-local.sh --predecessor,
#                  sp-oskp7): the release under test there was never uploaded, and the
#                  container has no forge credential. Needs a named tag, which the .tags
#                  sidecar records; never implied, never read from config.
#
#   --allow-draft  deploy a NAMED release even though it is still a draft. For release
#                  acceptance only: a release is published after it passes acceptance, and
#                  acceptance's upgrade and aged-upgrade phases deploy the release under test,
#                  so refusing its draft made them fail for every release by construction.
#                  Never implied, never read from config; "latest" still resolves to the
#                  newest PUBLISHED release with or without it.
#
# BOOTSTRAP
#   An instance whose checkout predates deploy.sh can bootstrap:
#     gh release download spira-release-<tag> --pattern <stem>.tar.gz --dir /tmp/
#     tar -xzf /tmp/<stem>.tar.gz -C /tmp/
#     SPIRA_CONF=/path/to/spira.conf bash /tmp/<stem>/spira/deploy.sh spira-release-<tag>
#   deploy.sh reads tools from its own directory ($HERE), not from the checkout, so it
#   does not require the checkout to carry a current copy of itself.
#
# FLOW
#   1. Resolve "latest" to the newest PUBLISHED (non-draft) spira-release-* release.
#   2. Refuse a draft release.
#   3. Refuse if the named release is already current.
#   4. Fetch the release tarball from the forge.
#   5. Check DB migration compatibility before drain.
#   6. Stop the promote timer (retired by this command).
#   7. Pre-deploy health check: run doctor.sh; refuse if any FAIL (pre-existing issue, not release).
#   8. world.sh drain — wait for live aeons to finish; refuse if they do not.
#      With --force: drain --timeout 0, then slay each live aeon (--keep-work --reopen).
#   9. Write SPIRA_PROD to spira.conf before restarting services.
#  10. activate.sh — unpack, atomic symlink swap, daemon-reload, restart units.
#  11. activated release's install.sh — re-render unit files with SPIRA_HOME and SPIRA_PROD
#      set to the release, so unit ExecStart paths are not stamped from the invoking directory.
#  12. cockpit/layout.sh ensure.
#  13. world.sh resume.
#  14. Health check: world.sh status, doctor.sh, skew.sh check.
#      On failure: restore prior state, restart, resume, exit 1 naming what failed.
#
# EXIT
#   0  deployed
#   1  refused (already current, drain timeout without --force, migration mismatch, health-check rollback)
#   2  fatal (usage error, draft release, fetch failed, activation error)
# covers: spira/deploy.sh spira/activate.sh spira/world.sh cockpit/layout.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/conf.sh"
. "$HERE/lib.sh"

# On an artifact deployment SPIRA_REPO is an extracted tarball with no .git, so gh
# cannot infer the repository from the working directory. SPIRA_GH_REPO (owner/repo)
# lets gh find the forge without a git context; GH_REPO is gh's built-in override.
if [ -z "${GH_REPO:-}" ] && [ -n "${SPIRA_GH_REPO:-}" ]; then
    export GH_REPO="$SPIRA_GH_REPO"
fi

_SC="${SPIRA_SYSTEMCTL:-systemctl}"
_WORLD="${SPIRA_WORLD_SH:-$HERE/world.sh}"
_ACTIVATE="${SPIRA_ACTIVATE_SH:-$HERE/activate.sh}"
_INSTALL="${SPIRA_INSTALL_SH:-$HERE/../systemd/install.sh}"
_COCKPIT="${SPIRA_COCKPIT_LAYOUT_SH:-$HERE/../cockpit/layout.sh}"
_DOCTOR="${SPIRA_DOCTOR_SH:-$HERE/doctor.sh}"
_SKEW="${SPIRA_SKEW_SH:-$HERE/skew.sh}"
_SLAY="${SPIRA_SLAY_SH:-$HERE/slay.sh}"
_CTRL="${SPIRA_CTRL_SH:-$HERE/ctrl.sh}"

# Save the pre-deploy production directory; first-deploy rollback restores units here.
_orig_prod="${SPIRA_PROD:-$SPIRA_HOME}"

dry_run=0
force=0
allow_draft=0
local_tarball=""
tag=""
while [ $# -gt 0 ]; do
    _a="$1"; shift
    case "$_a" in
        --dry-run) dry_run=1 ;;
        --force)   force=1 ;;
        --allow-draft) allow_draft=1 ;;
        --tarball)   local_tarball="${1:-}"; shift || true
                     [ -n "$local_tarball" ] || { printf 'deploy: --tarball needs a path\n' >&2; exit 2; } ;;
        --tarball=*) local_tarball="${_a#--tarball=}" ;;
        -*) printf 'deploy: unknown option: %s\n' "$_a" >&2; exit 2 ;;
        *)  [ -z "$tag" ] && tag="$_a" \
                || { printf 'deploy: too many arguments\n' >&2; exit 2; } ;;
    esac
done
unset _a
[ -n "$tag" ] || { printf 'usage: deploy.sh [--dry-run] [--force] [--allow-draft] [--tarball <path>] <tag|latest>\n' >&2; exit 2; }
if [ -n "$local_tarball" ]; then
    [ "$tag" != latest ] || {
        printf 'deploy: --tarball deploys a NAMED release; "latest" is resolved from the forge\n' >&2; exit 2; }
    [ -f "$local_tarball" ] || {
        printf 'deploy: --tarball: no such file: %s\n' "$local_tarball" >&2; exit 2; }
    case "$(basename "$local_tarball")" in
        spira-*.tar.gz) ;;
        *) printf 'deploy: --tarball: %s is not a spira-*.tar.gz release tarball\n' "$local_tarball" >&2; exit 2 ;;
    esac
    local_tarball="$(cd "$(dirname "$local_tarball")" && pwd -P)/$(basename "$local_tarball")"
fi

# Resolve the forge repository identifier for --repo on all gh calls.
# Required when SPIRA_REPO is an extracted tarball with no .git; also used for normal
# checkouts so every gh call is explicit about its target.
_gh_repo="${SPIRA_FORGE_REPO:-}"
if [ -z "$_gh_repo" ]; then
    _remote="$(git -C "$SPIRA_REPO" remote get-url origin 2>/dev/null)" || _remote=""
    case "$_remote" in
        https://github.com/*) _gh_repo="${_remote#https://github.com/}"; _gh_repo="${_gh_repo%.git}" ;;
        git@github.com:*)     _gh_repo="${_remote#git@github.com:}"; _gh_repo="${_gh_repo%.git}" ;;
    esac
    unset _remote
fi
[ -n "${_gh_repo:-}" ] || _gh_repo="${GH_REPO:-}"
[ -n "$_gh_repo" ] || [ -n "$local_tarball" ] || {
    printf 'deploy: SPIRA_FORGE_REPO is not set and forge repository cannot be inferred from git remote\n' >&2
    exit 2
}

# Resolve "latest" to the newest PUBLISHED (non-draft) spira-release-* release.
# gh release list skips drafts by filtering isDraft; fall back to git tags if gh unavailable.
_named_tag=1
if [ "$tag" = "latest" ]; then
    _named_tag=0
    tag=""
    git -C "$SPIRA_REPO" fetch --tags --quiet 2>/dev/null || true
    _rel_list="$(gh --repo "$_gh_repo" release list --json tagName,isDraft 2>/dev/null)" \
        || _rel_list=""
    if [ -n "$_rel_list" ]; then
        tag="$(printf '%s' "$_rel_list" | python3 -c '
import json, sys
try:
    releases = json.load(sys.stdin)
    pub = [r["tagName"] for r in releases
           if not r.get("isDraft", True)
           and r["tagName"].startswith("spira-release-")]
    pub.sort(reverse=True)
    print(pub[0] if pub else "")
except Exception:
    pass
' 2>/dev/null)" || tag=""
    fi
    if [ -z "${tag:-}" ]; then
        if [ -d "$SPIRA_REPO/.git" ] || [ -f "$SPIRA_REPO/.git" ]; then
            tag="$(git -C "$SPIRA_REPO" tag --list 'spira-release-*' \
                    --sort=-version:refname 2>/dev/null | head -1)"
        else
            tag="$(git ls-remote --tags "https://github.com/$_gh_repo" \
                    'refs/tags/spira-release-*' 2>/dev/null \
                | awk '{print $2}' | sed 's|refs/tags/||' \
                | sort -V | tail -1)"
        fi
    fi
    [ -n "$tag" ] || {
        printf 'deploy: no published spira-release-* release found\n' >&2; exit 2
    }
    log "deploy: latest = $tag"
fi

# Validate tag format before any remote calls.
_tag_stem="${tag#spira-release-}"
[ -n "$_tag_stem" ] && [ "$_tag_stem" != "$tag" ] || {
    printf 'deploy: tag %s does not match spira-release-<stem> format\n' "$tag" >&2
    exit 2
}
unset _tag_stem

[ -n "${SPIRA_RELEASES:-}" ] || {
    printf 'deploy: SPIRA_RELEASES is not set\n' >&2; exit 2
}

if [ -n "$local_tarball" ]; then
    log "deploy: $tag from the local tarball $local_tarball (--tarball) — no forge call"
fi
# Refuse a draft release before any disruptive action.
_draft_info=""
[ -n "$local_tarball" ] \
    || _draft_info="$(gh --repo "$_gh_repo" release view "$tag" --json isDraft 2>/dev/null)" \
    || _draft_info=""
if [ -n "$_draft_info" ]; then
    _is_draft="$(printf '%s' "$_draft_info" \
        | python3 -c 'import json,sys; print("yes" if json.load(sys.stdin).get("isDraft") else "no")' \
        2>/dev/null)" || _is_draft=""
    if [ "${_is_draft:-no}" = "yes" ]; then
        if [ "$allow_draft" = 1 ]; then
            log "deploy: $tag is a draft release — deploying it anyway (--allow-draft)"
        else
            printf 'deploy: %s is a draft release — publish it first\n' "$tag" >&2; exit 2
        fi
    fi
fi
unset _draft_info _is_draft

# Resolve the asset from the release. The tarball timestamp may differ from the tag
# timestamp; the asset name is authoritative. Refuse if there is not exactly one match.
_assets_json=""
if [ -n "$local_tarball" ]; then
    _assets_json="$(printf '{"assets":[{"name":"%s"}]}' "$(basename "$local_tarball")")"
else
    _assets_json="$(gh --repo "$_gh_repo" release view "$tag" --json assets 2>/dev/null)" \
        || _assets_json=""
fi
_asset_name="$(printf '%s' "${_assets_json:-}" | python3 -c '
import json,sys
try:
    assets = json.load(sys.stdin).get("assets", [])
    spira = [a["name"] for a in assets
             if a["name"].startswith("spira-") and a["name"].endswith(".tar.gz")]
    print(spira[0] if len(spira) == 1 else "")
except Exception:
    pass
' 2>/dev/null)" || _asset_name=""
[ -n "${_asset_name:-}" ] || {
    printf 'deploy: no unique spira-*.tar.gz asset found in release %s\n' "$tag" >&2; exit 2
}
release_stem="${_asset_name%.tar.gz}"
unset _assets_json _asset_name

# Bootstrap: if current is absent and SPIRA_PROD resolves inside SPIRA_RELEASES, this is
# a release-mode instance that has never been deployed through deploy.sh. Create
# current -> that release so the rest of the flow has a consistent prev_release to roll back to.
prev_release=""
if [ ! -L "$SPIRA_RELEASES/current" ] && [ -n "${SPIRA_PROD:-}" ]; then
    _prod_parent="$(dirname "${SPIRA_PROD}")"
    _rel_canon="$(cd "$SPIRA_RELEASES" 2>/dev/null && pwd -P)" || _rel_canon="$SPIRA_RELEASES"
    case "$_prod_parent/" in
        "$_rel_canon/"*)
            _boot_rel="${_prod_parent#$_rel_canon/}"
            case "$_boot_rel" in
                */*) : ;;
                *)
                    if [ -n "$_boot_rel" ] && [ -d "$SPIRA_RELEASES/$_boot_rel" ]; then
                        _tmp="$SPIRA_RELEASES/.current.bootstrap.$$"
                        ln -s "$_boot_rel" "$_tmp" \
                            && mv -T "$_tmp" "$SPIRA_RELEASES/current" \
                            && log "deploy: bootstrap: current -> $_boot_rel" \
                            || true
                    fi
                    ;;
            esac
            ;;
    esac
    unset _prod_parent _rel_canon _boot_rel _tmp
fi

# Refuse if this release is already current; in dry-run fall through to report.
if [ -L "$SPIRA_RELEASES/current" ]; then
    _current="$(readlink "$SPIRA_RELEASES/current")"
    if [ "$_current" = "$release_stem" ] && [ "$dry_run" != 1 ]; then
        printf 'deploy: %s is already current — nothing to do\n' "$release_stem" >&2
        exit 1
    fi
    prev_release="${_current:-}"
fi
unset _current

log "deploy: target $release_stem (prev: ${prev_release:-none})"

if [ "$dry_run" = 1 ]; then
    printf 'deploy: --dry-run — nothing will be changed\n'
    printf 'deploy: would install asset: %s.tar.gz\n' "$release_stem"
    printf 'deploy: would check DB migration compatibility\n'
    if [ "$force" = 1 ]; then
        printf 'deploy: would world.sh drain --timeout 0, then slay live aeons (--force)\n'
    else
        printf 'deploy: would world.sh drain\n'
    fi
    printf 'deploy: would update spira.conf: SPIRA_PROD=%s/current/spira\n' "$SPIRA_RELEASES"
    printf 'deploy: would activate.sh %s.tar.gz\n' "$release_stem"
    # When a current release is active, verify the ExecStart target that install.sh would
    # render is executable. This catches a wrong SPIRA_PROD before any disruptive action.
    if [ -L "$SPIRA_RELEASES/current" ]; then
        _dry_exec="$SPIRA_RELEASES/current/spira/sentinel.sh"
        printf 'deploy: ExecStart=%s\n' "$_dry_exec"
        if [ ! -x "$_dry_exec" ]; then
            printf 'deploy: dry-run: ExecStart target is not executable: %s\n' \
                "$_dry_exec" >&2
            exit 1
        fi
        unset _dry_exec
    fi
    # Verify the tag sidecar directory is or can be made writable before any disruptive action.
    _dry_tags="$SPIRA_RELEASES/.tags"
    mkdir -p "$_dry_tags" 2>/dev/null || true
    _dry_probe="$_dry_tags/.probe.$$"
    if ! printf '' > "$_dry_probe" 2>/dev/null; then
        printf 'deploy: dry-run: tag sidecar directory not writable: %s\n' "$_dry_tags" >&2
        exit 1
    fi
    rm -f "$_dry_probe"
    unset _dry_tags _dry_probe
    printf 'deploy: would systemd/install.sh (re-render units with SPIRA_PROD=%s/current/spira)\n' \
        "$SPIRA_RELEASES"
    printf 'deploy: would cockpit/layout.sh ensure\n'
    printf 'deploy: would world.sh resume\n'
    printf 'deploy: would run health checks\n'
    exit 0
fi

# Fetch the tarball from the forge.
mkdir -p "$SPIRA_RUN"
_deploy_tmp="$(mktemp -d "$SPIRA_RUN/deploy-XXXXXXXX")"
trap 'rm -rf "$_deploy_tmp"' EXIT

if [ -n "$local_tarball" ]; then
    cp "$local_tarball" "$_deploy_tmp/${release_stem}.tar.gz" || {
        printf 'deploy: could not copy %s\n' "$local_tarball" >&2; exit 2; }
else
    log "deploy: fetching $tag"
    gh --repo "$_gh_repo" release download "$tag" \
        --pattern "${release_stem}.tar.gz" \
        --dir "$_deploy_tmp" || {
        printf 'deploy: fetch failed\n' >&2; exit 2
    }
fi
_tarball="$_deploy_tmp/${release_stem}.tar.gz"
[ -f "$_tarball" ] || {
    printf 'deploy: tarball not found after download: %s\n' "$_tarball" >&2; exit 2
}

# Check DB migration compatibility before drain so a mismatch does not strand the instance.
if [ -d "${SPIRA_DB:-}/.beads" ]; then
    log "deploy: checking DB migration compatibility"
    _mig_out="$(timeout 30 "${SPIRA_BD:-bd}" -C "$SPIRA_DB" migrate schema 2>&1)"
    _mig_rc=$?
    if [ "$_mig_rc" -ne 0 ] \
            && printf '%s' "$_mig_out" | grep -qE 'unreachable|connection refused'; then
        log "deploy: Dolt server not running — starting"
        "${SPIRA_BD:-bd}" -C "$SPIRA_DB" dolt start 2>/dev/null || true
        _mig_out="$(timeout 30 "${SPIRA_BD:-bd}" -C "$SPIRA_DB" migrate schema 2>&1)"
        _mig_rc=$?
    fi
    if [ "$_mig_rc" -ne 0 ]; then
        if printf '%s' "$_mig_out" | grep -qE 'unreachable|connection refused'; then
            _dolt_addr="$(printf '%s' "$_mig_out" \
                | sed -n 's/.*unreachable at \([^ :]*:[0-9]*\).*/\1/p' | head -1)"
            printf 'deploy: cannot read DB migration count — Dolt server unreachable at %s\n' \
                "${_dolt_addr:-127.0.0.1:3307}" >&2
            printf '%s\n' "$_mig_out" >&2
        else
            printf 'deploy: DB migration mismatch — refusing before drain\n' >&2
            printf '%s\n' "$_mig_out" >&2
        fi
        exit 1
    fi
    unset _mig_out _mig_rc _dolt_addr
fi

# Retire the promote timer before draining; it is superseded by this command.
_promo_tmr="spira-promote-${SPIRA_INSTANCE}.timer"
_promo_svc="spira-promote-${SPIRA_INSTANCE}.service"
"$_SC" --user stop "$_promo_tmr" 2>/dev/null || true
"$_SC" --user disable "$_promo_tmr" 2>/dev/null || true
"$_SC" --user stop "$_promo_svc" 2>/dev/null || true

# Ensure the tag sidecar directory exists and is writable before disrupting the instance.
# The release dir is read-only after activate.sh; the sidecar goes beside it, not inside.
_tags_dir="$SPIRA_RELEASES/.tags"
mkdir -p "$_tags_dir" || {
    printf 'deploy: cannot create tag sidecar directory %s\n' "$_tags_dir" >&2; exit 1
}
_tags_probe="$_tags_dir/.probe.$$"
printf '' > "$_tags_probe" || {
    printf 'deploy: tag sidecar directory %s is not writable\n' "$_tags_dir" >&2; exit 1
}
rm -f "$_tags_probe"
unset _tags_dir _tags_probe

# Snapshot which spira units are currently enabled. Rollback uses this to restore units
# that the incoming release drops from its manifest (they get pruned by the new install.sh
# and left disabled, then the rollback's install.sh sees them as operator-disabled).
_pre_deploy_unit_state="$SPIRA_RUN/pre-deploy-unit-state.$$"
"$_SC" --user list-unit-files --no-legend \
    "spira-*-${SPIRA_INSTANCE}.service" \
    "spira-*-${SPIRA_INSTANCE}.timer" 2>/dev/null \
    > "$_pre_deploy_unit_state" 2>/dev/null || true

# Pre-deploy health baseline: run the incoming doctor before any disruptive step so
# fatals that exist before activation are named here, not blamed on the release.
# A fatal here means the box has a pre-existing problem; fix it and re-run deploy.
log "deploy: pre-deploy health check"
_pre_deploy_fails="$(SPIRA_DOCTOR=1 "$_DOCTOR" 2>&1 | grep '^  FAIL  ')" || true
if [ -n "$_pre_deploy_fails" ]; then
    printf 'deploy: pre-deploy health check has failures — fix before deploying:\n' >&2
    printf '%s\n' "$_pre_deploy_fails" >&2
    exit 1
fi
unset _pre_deploy_fails

# Drain: wait for live aeons to finish; refuse if they do not.
# With --force: set the gate immediately, then slay any remaining aeons.
log "deploy: draining"
if [ "$force" = 1 ]; then
    "$_WORLD" drain --timeout 0 2>/dev/null || true
    for _pf in "$SPIRA_RUN"/aeon-*.pid; do
        [ -e "$_pf" ] || continue
        _apid="$(cat "$_pf" 2>/dev/null)"
        [ -n "$_apid" ] && [ -d "/proc/$_apid" ] || { rm -f "$_pf"; continue; }
        _abead="$(basename "$_pf" .pid)"; _abead="${_abead#aeon-}"; _abead="${_abead#*-}"
        [ -n "$_abead" ] || continue
        log "deploy: --force: slaying $_abead"
        "$_SLAY" --bead "$_abead" --keep-work --reopen --why "deploy $tag" || true
    done
    unset _pf _apid _abead
else
    "$_WORLD" drain || {
        printf 'deploy: drain refused — live aeons did not finish in time\n' >&2
        exit 1
    }
fi

# _render_release_units — re-render every unit for the release $SPIRA_RELEASES/current points
# at, through THAT release's install.sh, resolving THAT release's binaries.
#
# THE RELEASE, NOT THE INVOKING CHECKOUT. This script sourced conf.sh, which derived and
# EXPORTED SPIRA_BROKER_BIN, SPIRA_LOOM_BIN, ... (and SPIRA_WAKE) from the SPIRA_REPO it was
# run from, and the release's install.sh kept them — conf.sh only fills keys that are unset.
# Run from a source checkout with nothing built (acceptance, or an operator's clone), units.sh
# found no broker or loom binary and PRUNED spira-broker and spira-loom on every deploy and
# every rollback; acceptance phase C's rollback then failed its health check on "loom does
# not answer" and restored the newer release (2026-09-26). SPIRA_REPO is the release, and the
# derived values are cleared so the release's own conf.sh derives them from its bin/; a value
# the operator set in spira.conf is read again by that conf.sh, so nothing explicit is lost.
_render_release_units() {
    env -u SPIRA_LOOM_BIN -u SPIRA_BROKER_BIN -u SPIRA_CZAR_PASS_BIN -u SPIRA_QUEUE_WATCH_BIN \
        -u SPIRA_RECONCILER_BIN -u SPIRA_SUPERVISE_BIN -u SPIRA_LANDING_PASS_BIN \
        -u SPIRA_TSD_BIN -u SPIRA_BATCHER_BIN -u SPIRA_TEST_PLAN_BIN -u SPIRA_PANEL -u SPIRA_WAKE \
        SPIRA_REPO="$SPIRA_RELEASES/current" \
        SPIRA_HOME="$SPIRA_RELEASES/current/spira" \
        SPIRA_PROD="$SPIRA_RELEASES/current/spira" SPIRA_INSTALL_FORCE=1 \
        bash "${SPIRA_INSTALL_SH:-$SPIRA_RELEASES/current/systemd/install.sh}"
}

# Rollback: restore prior state, restart, resume.
# On a non-first deploy: swap current back to the prior release.
# On a first deploy: remove current and re-render units against the original checkout.
_conf_path=""
_conf_backup=""
_rollback() {
    local _why="$1"
    printf 'deploy: ROLLBACK — %s\n' "$_why" >&2
    [ -z "${_conf_backup:-}" ] || [ ! -f "${_conf_backup:-}" ] || \
        mv "${_conf_backup}" "${_conf_path}" 2>/dev/null || \
        log "deploy: rollback: could not restore spira.conf"
    if [ -n "$prev_release" ] && [ -d "$SPIRA_RELEASES/$prev_release" ]; then
        local _tmp="$SPIRA_RELEASES/.current.rollback.$$"
        ln -s "$prev_release" "$_tmp" && mv -T "$_tmp" "$SPIRA_RELEASES/current" || {
            printf 'deploy: rollback: symlink swap failed\n' >&2
        }
        # Re-render units against the prior release, pruning units the newer release added.
        # current now points at the prior release; use its install.sh so SPIRA_HOME is right.
        _render_release_units 2>/dev/null || true
        # Restore units that were enabled before the deploy but that the incoming release's
        # install disabled (by pruning them from its manifest). install.sh treats a disabled
        # unit as operator-disabled and leaves it alone, so we must restore from the snapshot.
        if [ -f "${_pre_deploy_unit_state:-}" ]; then
            while read -r _pdu_u _pdu_s _pdu_preset; do
                [ "${_pdu_s}" = "enabled" ] || continue
                _pdu_cs="${_pdu_u%"-${SPIRA_INSTANCE}.service"}"; _pdu_cs="${_pdu_cs%"-${SPIRA_INSTANCE}.timer"}"
                _pdu_cs="${_pdu_cs%.service}"; _pdu_cs="${_pdu_cs%.timer}"
                if [ -x "$_CTRL" ] && "$_CTRL" check "$_pdu_cs" >/dev/null 2>&1; then
                    printf 'deploy: rollback: %s is suspended — skipping\n' "$_pdu_u" >&2
                    continue
                fi
                _pdu_now="$("$_SC" --user is-enabled "$_pdu_u" 2>/dev/null || true)"
                [ "$_pdu_now" = "disabled" ] || continue
                "$_SC" --user enable --now "$_pdu_u" 2>/dev/null || true
            done < "${_pre_deploy_unit_state}"
            rm -f "${_pre_deploy_unit_state}" 2>/dev/null || true
        fi
        # ExecStart is parameterized by the "current" symlink, never a release name, so the
        # re-render above is a no-op diff and install.sh neither reloads nor restarts anything —
        # already-running units must be restarted onto the prior release explicitly here.
        "$_SC" --user daemon-reload 2>/dev/null || true
        "$_SC" --user list-units "spira-*-${SPIRA_INSTANCE}.service" \
            --state=active --no-legend 2>/dev/null \
            | awk '{print $1}' | grep -v "spira-aeon-" \
            | xargs -r "$_SC" --user restart 2>/dev/null || true
        "$_WORLD" resume 2>/dev/null || true
        printf 'deploy: restored %s\n' "$prev_release" >&2
    else
        # First deploy: remove current so nothing points at the failed release.
        rm -f "$SPIRA_RELEASES/current" 2>/dev/null || true
        SPIRA_PROD="$_orig_prod" SPIRA_INSTALL_FORCE=1 bash "$_INSTALL" 2>/dev/null || true
        "$_SC" --user daemon-reload 2>/dev/null || true
        "$_SC" --user list-units "spira-*-${SPIRA_INSTANCE}.service" \
            --state=active --no-legend 2>/dev/null \
            | awk '{print $1}' | grep -v "spira-aeon-" \
            | xargs -r "$_SC" --user restart 2>/dev/null || true
        "$_WORLD" resume 2>/dev/null || true
        printf 'deploy: no prior release — units restored to checkout\n' >&2
    fi
    exit 1
}

# Write SPIRA_PROD to spira.conf before restarting services so they come up reading the
# current config. Rollback restores the backup if activation fails.
_conf_path="$(spira_conf_file)"
[ -n "$_conf_path" ] || _conf_path="$SPIRA_REPO/spira.conf"
if [ -f "$_conf_path" ]; then
    _conf_backup="${_conf_path}.pre-deploy.$$"
    cp "$_conf_path" "$_conf_backup" 2>/dev/null || _conf_backup=""
    grep -v '^SPIRA_PROD[[:space:]]*=' "$_conf_path" > "${_conf_path}.new.$$" 2>/dev/null \
        || :> "${_conf_path}.new.$$"
else
    :> "${_conf_path}.new.$$"
fi
printf 'SPIRA_PROD = %s/current/spira\n' "$SPIRA_RELEASES" >> "${_conf_path}.new.$$"
mv "${_conf_path}.new.$$" "$_conf_path" \
    || log "deploy: WARN: could not write SPIRA_PROD to $_conf_path — fix manually"
log "deploy: spira.conf updated — SPIRA_PROD = $SPIRA_RELEASES/current/spira"

# Activate the tarball.
log "deploy: activating"
"$_ACTIVATE" "$_tarball" || { _rollback "activate.sh failed"; }

# Record the release tag in .tags/ (beside the release dirs, not inside the read-only one).
printf '%s\n' "$tag" > "$SPIRA_RELEASES/.tags/$release_stem" || {
    _rollback "sidecar write failed: $SPIRA_RELEASES/.tags/$release_stem"
}

# Re-render unit files so ExecStart paths point at $SPIRA_RELEASES/current/spira.
# One-time cutover if SPIRA_PROD was previously set to the checkout; idempotent thereafter.
# Use the activated release's own install.sh so conf.sh is sourced from the correct location;
# a deploy.sh invoked from a temporary directory would otherwise stamp that directory into
# SPIRA_HOME across all 24+ unit ExecStart lines.
log "deploy: re-rendering units"
_render_release_units || {
    _rollback "unit re-render failed"
}

# A UNIT THE TARGET RELEASE PRUNED LEAVES NO FAILED GHOST. `current` is swapped before the
# target's installer prunes the units it does not know, so on a deploy of an OLDER release a
# timer of such a unit can fire in between, fail to exec its binary (203/EXEC) and stay failed
# as "not-found failed" once its file is gone — and doctor then failed this deploy's health
# check (acceptance phase C: spira-reconciler-flow, 2026-09-27). The unit is not installed
# any more, so its failure is not the release's: clear it. A failed unit whose file IS still
# installed is a real failure and is left for doctor. This runs from this checkout, so it
# holds whatever the target release's own installer knows.
"$_SC" --user list-units --state=failed --all --no-legend --plain \
        "spira-*-${SPIRA_INSTANCE}.service" "spira-*-${SPIRA_INSTANCE}.timer" 2>/dev/null \
    | awk '$2 == "not-found" {print $1}' \
    | while IFS= read -r _ghost; do
        [ -n "$_ghost" ] || continue
        "$_SC" --user reset-failed "$_ghost" 2>/dev/null \
            && log "deploy: cleared the failed state of $_ghost — the release in force does not install it"
    done

# Ensure cockpit layout is current.
log "deploy: ensuring cockpit"
"$_COCKPIT" ensure 2>/dev/null || true

# Remove the drain stamp so aeons can be summoned again.
log "deploy: resuming"
"$_WORLD" resume

# A UNIT THE DEPLOY ITSELF KNOCKED OVER IS RE-RUN, NOT JUDGED. The pre-deploy health check
# refused to start on failed units, so any installed unit failed now failed INSIDE this
# window — and this deploy restarts dolt-beads while the timers keep firing: spira-sentinel
# fired the same second the re-render restarted dolt, lost its database, exited 1, and doctor
# rolled a healthy release back (acceptance phase B, 2026-09-27). Reset each one and start it
# once under the release in force; a unit the release genuinely breaks fails again and stays
# failed, and doctor below judges it exactly as before.
"$_SC" --user list-units --state=failed --all --no-legend --plain \
        "spira-*-${SPIRA_INSTANCE}.service" 2>/dev/null \
    | awk '$2 == "loaded" {print $1}' \
    | while IFS= read -r _knocked; do
        [ -n "$_knocked" ] || continue
        "$_SC" --user reset-failed "$_knocked" 2>/dev/null || true
        if timeout "${SPIRA_DEPLOY_RERUN_TIMEOUT:-300}" "$_SC" --user start "$_knocked" 2>/dev/null; then
            log "deploy: re-ran $_knocked — it failed inside the deploy window and passes under $release_stem"
        else
            log "deploy: $_knocked fails again under $release_stem — left failed for the health check"
        fi
    done

# Health check: verify the activated release is up and healthy.
log "deploy: health check"
_deploy_failed=""
"$_WORLD" status >/dev/null 2>&1 \
    || _deploy_failed="${_deploy_failed:+$_deploy_failed, }world status"
_skew_out="$("$_SKEW" check 2>/dev/null)"
_skew_exit=$?
# AN OLDER RELEASE THE OPERATOR NAMED IS NOT-LATEST BY CONSTRUCTION. skew.sh check answers
# NOT-LATEST (exit 1) whenever a newer release tag exists — exactly the state a deliberate
# rollback to a named release produces — and reading it as a failed health check undid every
# such rollback (acceptance phase C, 2026-09-26). Accepted ONLY when the finding is nothing
# but NOT-LATEST, the operator named $tag explicitly, and the release skew calls latest is a
# DIFFERENT tag that sorts AFTER $tag: a genuinely newer release exists. A NOT-LATEST about the
# release this deploy just made newest (skew misreading its own sidecar), MANIFEST-MISMATCH,
# or NOT-LATEST after `latest` still fails.
_skew_newer=""
if [ "$_skew_exit" -eq 1 ] && [ "$_named_tag" = 1 ] \
   && [ -z "$(printf '%s\n' "$_skew_out" | grep -v '^NOT-LATEST ' | grep -v '^[[:space:]]*$' || true)" ]; then
    _skew_newer="$(printf '%s\n' "$_skew_out" \
        | sed -n 's/^NOT-LATEST .* is not the latest published release \(spira-release-[^ ]*\)$/\1/p' | head -1)"
    if [ -n "$_skew_newer" ] && [ "$_skew_newer" != "$tag" ] \
       && [ "$(printf '%s\n%s\n' "$tag" "$_skew_newer" | sort | tail -1)" = "$_skew_newer" ]; then
        log "deploy: skew: NOT-LATEST — a newer release ($_skew_newer) exists; expected, $tag was named explicitly"
        _skew_exit=0
    fi
fi
unset _skew_newer
# THE SKEW UNIT IS THE SAME CHECK, ON A TIMER. Under the named older release its own
# spira-skew unit runs skew.sh check, gets the same NOT-LATEST, exits 1 and sits failed; the
# re-run above cannot help, because it fails again for the same expected reason, and doctor
# then counted it as a failed unit and rolled the deliberate rollback back (acceptance
# phase C, 2026-09-27: "spira-skew-prod.service fails again … ROLLBACK — doctor"). The
# finding was accepted just above, so its unit's failure is that accepted finding: clear it
# before doctor judges the rest. Only on acceptance — any other skew finding leaves it failed.
if [ "$_skew_exit" -eq 0 ] && [ "$_named_tag" = 1 ]; then
    "$_SC" --user reset-failed "spira-skew-${SPIRA_INSTANCE}.service" 2>/dev/null || true
fi
# THE FAIL LINES ARE THE EVIDENCE. A bare "ROLLBACK — doctor" says a check failed and not
# which; the rollback that follows destroys the state it failed on. Print what doctor said.
if ! _doctor_out="$(SPIRA_DOCTOR=1 "$_DOCTOR" 2>&1)"; then
    _deploy_failed="${_deploy_failed:+$_deploy_failed, }doctor"
    printf '%s\n' "$_doctor_out" | grep -E '^\s*FAIL' | sed 's/^/deploy: doctor: /' >&2
fi
unset _doctor_out
# THE SKEW FINDINGS ARE THE EVIDENCE, as doctor's FAIL lines are above. A bare "ROLLBACK — skew
# (exit 1)" hid which finding rejected the deploy, and the rollback that followed destroyed the state
# it was judged on (acceptance phase C, run 36296891710, 2026-09-27).
if [ "$_skew_exit" -ne 0 ]; then
    _deploy_failed="${_deploy_failed:+$_deploy_failed, }skew (exit $_skew_exit)"
    printf '%s\n' "$_skew_out" | grep -v '^[[:space:]]*$' | sed 's/^/deploy: skew: /' >&2
fi

[ -z "$_deploy_failed" ] || { _rollback "$_deploy_failed"; }

rm -f "${_pre_deploy_unit_state:-}" 2>/dev/null || true
log "deploy: $release_stem active"
