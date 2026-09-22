#!/usr/bin/env bash
#
# deploy.sh — operator-run release deploy.
#
#   deploy.sh [--dry-run] [--force] <tag|latest>
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
#   7. world.sh drain — wait for live aeons to finish; refuse if they do not.
#      With --force: drain --timeout 0, then slay each live aeon (--keep-work --reopen).
#   8. Write SPIRA_PROD to spira.conf before restarting services.
#   9. activate.sh — unpack, atomic symlink swap, daemon-reload, restart units.
#  10. systemd/install.sh — re-render unit files with SPIRA_PROD=$SPIRA_RELEASES/current/spira.
#  11. cockpit/layout.sh ensure.
#  12. world.sh resume.
#  13. Health check: world.sh status, doctor.sh, skew.sh check.
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
tag=""
for _a in "$@"; do
    case "$_a" in
        --dry-run) dry_run=1 ;;
        --force)   force=1 ;;
        -*) printf 'deploy: unknown option: %s\n' "$_a" >&2; exit 2 ;;
        *)  [ -z "$tag" ] && tag="$_a" \
                || { printf 'deploy: too many arguments\n' >&2; exit 2; } ;;
    esac
done
unset _a
[ -n "$tag" ] || { printf 'usage: deploy.sh [--dry-run] [--force] <tag|latest>\n' >&2; exit 2; }

# Resolve "latest" to the newest PUBLISHED (non-draft) spira-release-* release.
# gh release list skips drafts by filtering isDraft; fall back to git tags if gh unavailable.
if [ "$tag" = "latest" ]; then
    git -C "$SPIRA_REPO" fetch --tags --quiet 2>/dev/null || true
    _rel_list="$(cd "$SPIRA_REPO" && gh release list --json tagName,isDraft 2>/dev/null)" \
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
        tag="$(git -C "$SPIRA_REPO" tag --list 'spira-release-*' \
                --sort=-version:refname 2>/dev/null | head -1)"
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

# Refuse a draft release before any disruptive action.
_draft_info="$(cd "$SPIRA_REPO" && gh release view "$tag" --json isDraft 2>/dev/null)" \
    || _draft_info=""
if [ -n "$_draft_info" ]; then
    _is_draft="$(printf '%s' "$_draft_info" \
        | python3 -c 'import json,sys; print("yes" if json.load(sys.stdin).get("isDraft") else "no")' \
        2>/dev/null)" || _is_draft=""
    if [ "${_is_draft:-no}" = "yes" ]; then
        printf 'deploy: %s is a draft release — publish it first\n' "$tag" >&2; exit 2
    fi
fi
unset _draft_info _is_draft

# Resolve the asset from the release. The tarball timestamp may differ from the tag
# timestamp; the asset name is authoritative. Refuse if there is not exactly one match.
_assets_json="$(cd "$SPIRA_REPO" && gh release view "$tag" --json assets 2>/dev/null)" \
    || _assets_json=""
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

log "deploy: fetching $tag"
(cd "$SPIRA_REPO" && gh release download "$tag" \
    --pattern "${release_stem}.tar.gz" \
    --dir "$_deploy_tmp") || {
    printf 'deploy: fetch failed\n' >&2; exit 2
}
_tarball="$_deploy_tmp/${release_stem}.tar.gz"
[ -f "$_tarball" ] || {
    printf 'deploy: tarball not found after download: %s\n' "$_tarball" >&2; exit 2
}

# Check DB migration compatibility before drain so a mismatch does not strand the instance.
if [ -d "${SPIRA_DB:-}/.beads" ]; then
    log "deploy: checking DB migration compatibility"
    _mig_out="$(timeout 30 "${SPIRA_BD:-bd}" -C "$SPIRA_DB" migrate schema 2>&1)" || {
        printf 'deploy: DB migration mismatch — refusing before drain\n' >&2
        printf '%s\n' "$_mig_out" >&2
        exit 1
    }
    unset _mig_out
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
        SPIRA_PROD="$SPIRA_RELEASES/current/spira" SPIRA_INSTALL_FORCE=1 \
            bash "$_INSTALL" 2>/dev/null || true
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
log "deploy: re-rendering units"
SPIRA_PROD="$SPIRA_RELEASES/current/spira" SPIRA_INSTALL_FORCE=1 bash "$_INSTALL" || {
    _rollback "unit re-render failed"
}

# Ensure cockpit layout is current.
log "deploy: ensuring cockpit"
"$_COCKPIT" ensure 2>/dev/null || true

# Remove the drain stamp so aeons can be summoned again.
log "deploy: resuming"
"$_WORLD" resume

# Health check: verify the activated release is up and healthy.
log "deploy: health check"
_deploy_failed=""
"$_WORLD" status >/dev/null 2>&1 \
    || _deploy_failed="${_deploy_failed:+$_deploy_failed, }world status"
SPIRA_DOCTOR=1 "$_DOCTOR" >/dev/null 2>&1 \
    || _deploy_failed="${_deploy_failed:+$_deploy_failed, }doctor"
"$_SKEW" check >/dev/null 2>&1
_skew_exit=$?
[ "$_skew_exit" -eq 0 ] \
    || _deploy_failed="${_deploy_failed:+$_deploy_failed, }skew (exit $_skew_exit)"

[ -z "$_deploy_failed" ] || { _rollback "$_deploy_failed"; }

rm -f "${_pre_deploy_unit_state:-}" 2>/dev/null || true
log "deploy: $release_stem active"
