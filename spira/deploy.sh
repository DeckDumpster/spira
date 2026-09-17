#!/usr/bin/env bash
#
# deploy.sh — operator-run release deploy.
#
#   deploy.sh [--dry-run] <tag|latest>
#
# FLOW
#   1. Resolve "latest" to the newest spira-release-* tag.
#   2. Refuse if the named release is already current.
#   3. Fetch the release tarball from the forge.
#   4. Stop the promote timer (retired by this command).
#   5. world.sh drain — wait for live aeons to finish; refuse if they do not.
#   6. activate.sh — unpack, atomic symlink swap, daemon-reload, restart units.
#   7. systemd/install.sh — re-render unit files with SPIRA_PROD=$SPIRA_RELEASES/current.
#   8. cockpit/layout.sh ensure.
#   9. world.sh resume.
#  10. Health check: world.sh status, doctor.sh, skew.sh check.
#      On failure: swap current back, restart, resume, exit 1 naming what failed.
#
# EXIT
#   0  deployed
#   1  refused (already current, drain timeout, or health-check rollback)
#   2  fatal (usage error, fetch failed, activation error)
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

dry_run=0
tag=""
for _a in "$@"; do
    case "$_a" in
        --dry-run) dry_run=1 ;;
        -*) printf 'deploy: unknown option: %s\n' "$_a" >&2; exit 2 ;;
        *)  [ -z "$tag" ] && tag="$_a" \
                || { printf 'deploy: too many arguments\n' >&2; exit 2; } ;;
    esac
done
unset _a
[ -n "$tag" ] || { printf 'usage: deploy.sh [--dry-run] <tag|latest>\n' >&2; exit 2; }

# Resolve "latest" to the newest spira-release-* tag.
if [ "$tag" = "latest" ]; then
    git -C "$SPIRA_REPO" fetch --tags --quiet 2>/dev/null || true
    tag="$(git -C "$SPIRA_REPO" tag --list 'spira-release-*' \
            --sort=-version:refname 2>/dev/null | head -1)"
    [ -n "$tag" ] || {
        printf 'deploy: no spira-release-* tags found\n' >&2; exit 2
    }
    log "deploy: latest = $tag"
fi

# Derive the release directory name from the tag (spira-release-<stem> → <stem>).
release_stem="${tag#spira-release-}"
[ -n "$release_stem" ] && [ "$release_stem" != "$tag" ] || {
    printf 'deploy: tag %s does not match spira-release-<stem> format\n' "$tag" >&2
    exit 2
}

[ -n "${SPIRA_RELEASES:-}" ] || {
    printf 'deploy: SPIRA_RELEASES is not set\n' >&2; exit 2
}

# Refuse if this release is already current.
prev_release=""
if [ -L "$SPIRA_RELEASES/current" ]; then
    _current="$(readlink "$SPIRA_RELEASES/current")"
    if [ "$_current" = "$release_stem" ]; then
        printf 'deploy: %s is already current — nothing to do\n' "$release_stem" >&2
        exit 1
    fi
    prev_release="$_current"
fi
unset _current

log "deploy: target $release_stem (prev: ${prev_release:-none})"

if [ "$dry_run" = 1 ]; then
    printf 'deploy: --dry-run — nothing will be changed\n'
    printf 'deploy: would fetch %s from forge\n' "$tag"
    printf 'deploy: would world.sh drain\n'
    printf 'deploy: would activate.sh %s.tar.gz\n' "$release_stem"
    printf 'deploy: would systemd/install.sh (re-render units)\n'
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

# Retire the promote timer before draining; it is superseded by this command.
_promo_tmr="spira-promote-${SPIRA_INSTANCE}.timer"
_promo_svc="spira-promote-${SPIRA_INSTANCE}.service"
"$_SC" --user stop "$_promo_tmr" 2>/dev/null || true
"$_SC" --user disable "$_promo_tmr" 2>/dev/null || true
"$_SC" --user stop "$_promo_svc" 2>/dev/null || true

# Drain: wait for live aeons to finish; refuse if they do not.
log "deploy: draining"
"$_WORLD" drain || {
    printf 'deploy: drain refused — live aeons did not finish in time\n' >&2
    exit 1
}

# Rollback: swap current back to the prior release, restart, resume.
# Called after activate.sh has already swapped current; direct symlink swap
# avoids activate.sh's live-aeon guard (aeons are drained at this point).
_rollback() {
    local _why="$1"
    printf 'deploy: ROLLBACK — %s\n' "$_why" >&2
    if [ -n "$prev_release" ] && [ -d "$SPIRA_RELEASES/$prev_release" ]; then
        local _tmp="$SPIRA_RELEASES/.current.rollback.$$"
        ln -s "$prev_release" "$_tmp" && mv -T "$_tmp" "$SPIRA_RELEASES/current" || {
            printf 'deploy: rollback: symlink swap failed\n' >&2
        }
        "$_SC" --user daemon-reload 2>/dev/null || true
        "$_SC" --user list-units "spira-*-${SPIRA_INSTANCE}.service" \
            --state=active --no-legend 2>/dev/null \
            | awk '{print $1}' | grep -v "spira-aeon-" \
            | xargs -r "$_SC" --user restart 2>/dev/null || true
        "$_WORLD" resume 2>/dev/null || true
        printf 'deploy: restored %s\n' "$prev_release" >&2
    else
        printf 'deploy: no prior release to restore\n' >&2
    fi
    exit 1
}

# Activate the tarball.
log "deploy: activating"
"$_ACTIVATE" "$_tarball" || { _rollback "activate.sh failed"; }

# Re-render unit files so ExecStart paths point at $SPIRA_RELEASES/current.
# One-time cutover if SPIRA_PROD was previously set to the checkout; idempotent thereafter.
log "deploy: re-rendering units"
SPIRA_PROD="$SPIRA_RELEASES/current" SPIRA_INSTALL_FORCE=1 bash "$_INSTALL" || {
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

log "deploy: $release_stem active"
