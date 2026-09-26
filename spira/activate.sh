#!/usr/bin/env bash
#
# activate.sh — unpack a release tarball beside existing releases, swap the
#               current symlink atomically, daemon-reload, and restart.
#
# USAGE
#   activate.sh [--dry-run] <tarball>
#
# TARBALL FORMAT
#   spira-<YYYYMMDDTHHMMSSZ>.tar.gz as produced by build-tarball.sh. The tarball
#   must unpack to a top-level directory of the same name, e.g.:
#     spira-20260912T220000Z/
#   That naming convention is enforced by build-tarball.sh so the tag and the
#   artifact name each other; this script validates that the unpacked content
#   matches what the filename promised.
#
# FLOW
#   1. Refuse if aeons are live for this instance (unless SPIRA_ACTIVATE_FORCE=1).
#   2. Unpack the tarball into $SPIRA_RELEASES/<release-name>/ via a staging dir;
#      the staging dir is cleaned on any failure so a partial unpack never appears
#      as the release dir.
#   3. Make the release directory read-only (chmod -R a-w). A filesystem that refuses
#      is stronger than a rule that people should not edit production.
#   4. Atomically swap $SPIRA_RELEASES/current -> <release-name> via a temp symlink
#      + rename(2). At no point does current point at an incomplete tree.
#   5. daemon-reload so systemd picks up any unit-file changes in the new release.
#   6. Restart all active non-aeon spira-* service units for this instance.
#   7. Prune releases beyond $SPIRA_RELEASES_KEEP (default 100), best-effort. A prune
#      failure is reported but does NOT fail the activation. Prune runs AFTER the
#      swap and can never leave current pointing at nothing.
#
# ATOMICITY GUARANTEE
#   The release directory is unpacked completely and made read-only BEFORE the symlink
#   swings. Until the rename(2), the previous current is still in effect. After the
#   rename, current points at the new release. Rollback is: activate.sh <prev-tarball>.
#
# LIVE-AEON GUARD
#   Installing while aeons hold worktrees destroys in-flight work (scar: sp-s6hk,
#   33 reload entries coinciding with a worker death). The guard refuses while any
#   spira-aeon-*-<instance> unit is active. Set SPIRA_ACTIVATE_FORCE=1 to override,
#   e.g. after world.sh stop has confirmed all aeons are gone.
#
# EXIT
#   0   success
#   1   refused or failed (see stderr)
#   2   usage error
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/lib.sh"

SC="${SPIRA_SYSTEMCTL:-systemctl}"
DRY_RUN=0

while [ $# -gt 0 ]; do
    case "$1" in
        --dry-run) DRY_RUN=1; shift ;;
        --) shift; break ;;
        -*) printf 'activate: unknown flag: %s\n' "$1" >&2; exit 2 ;;
        *)  break ;;
    esac
done

TARBALL="${1:-}"
[ -n "$TARBALL" ] || { printf 'usage: activate.sh [--dry-run] <tarball>\n' >&2; exit 2; }
[ -f "$TARBALL" ] || { printf 'activate: tarball not found: %s\n' "$TARBALL" >&2; exit 1; }

# Derive the release name from the tarball filename.
# Expected: spira-<YYYYMMDDTHHMMSSZ>.tar.gz
BASENAME="$(basename "$TARBALL")"
case "$BASENAME" in
    *.tar.gz) RELEASE_NAME="${BASENAME%.tar.gz}" ;;
    *.tgz)    RELEASE_NAME="${BASENAME%.tgz}" ;;
    *)
        printf 'activate: expected a .tar.gz tarball, got: %s\n' "$BASENAME" >&2
        exit 2
        ;;
esac

case "$RELEASE_NAME" in
    spira-?*)  ;;
    *)
        printf 'activate: tarball must be named spira-<timestamp>.tar.gz, got: %s\n' \
            "$BASENAME" >&2
        exit 2
        ;;
esac

RELEASES="${SPIRA_RELEASES:-}"
[ -n "$RELEASES" ] || {
    printf 'activate: SPIRA_RELEASES is not set — set it in spira.conf\n' >&2
    exit 1
}
KEEP="${SPIRA_RELEASES_KEEP:-100}"
RELEASE_DIR="$RELEASES/$RELEASE_NAME"
CURRENT="$RELEASES/current"

# ---------------------------------------------------------------------------
# Live-aeon guard. Installing while aeons are active destroys their worktrees.
# ---------------------------------------------------------------------------
if [ -z "${SPIRA_ACTIVATE_FORCE:-}" ]; then
    _live="$(spira_live_aeons)"
    if [ -n "$_live" ]; then
        printf 'activate: refusing — live aeons for instance %s would be disrupted:\n' \
            "$SPIRA_INSTANCE" >&2
        printf '%s\n' "$_live" | sed 's/^/  /' >&2
        printf 'activate: wait for them to finish, or set SPIRA_ACTIVATE_FORCE=1 to override.\n' >&2
        exit 1
    fi
fi

# ---------------------------------------------------------------------------
# Dry run — report intent without touching the release directory.
# ---------------------------------------------------------------------------
if [ "$DRY_RUN" = 1 ]; then
    log "activate: DRY RUN: would unpack $TARBALL -> $RELEASE_DIR"
    log "activate: DRY RUN: would swap current -> $RELEASE_NAME"
    log "activate: DRY RUN: would daemon-reload and restart spira-* units"
    exit 0
fi

# ---------------------------------------------------------------------------
# Unpack unless this release is already installed; rollback skips to the swap.
# ---------------------------------------------------------------------------
if [ -e "$RELEASE_DIR" ]; then
    log "activate: $RELEASE_NAME already present — skipping unpack"
else
    mkdir -p "$RELEASES" || {
        printf 'activate: cannot create release directory: %s\n' "$RELEASES" >&2
        exit 1
    }

    UNPACK_TMP="$RELEASES/.unpack.$$"
    _cleanup_unpack() { rm -rf "$UNPACK_TMP" 2>/dev/null || true; }
    trap '_cleanup_unpack' EXIT INT TERM

    mkdir -p "$UNPACK_TMP"

    log "activate: unpacking $BASENAME"
    tar -xzf "$TARBALL" -C "$UNPACK_TMP" || {
        printf 'activate: failed to unpack tarball: %s\n' "$TARBALL" >&2
        exit 1
    }

    if [ ! -d "$UNPACK_TMP/$RELEASE_NAME" ]; then
        printf 'activate: tarball did not produce expected directory %s/\n' "$RELEASE_NAME" >&2
        printf 'activate: (searched in %s)\n' "$UNPACK_TMP" >&2
        exit 1
    fi

    mv "$UNPACK_TMP/$RELEASE_NAME" "$RELEASE_DIR"
    rmdir "$UNPACK_TMP" 2>/dev/null || true
    trap - EXIT INT TERM

    chmod -R a-w "$RELEASE_DIR" \
        || log "activate: WARN: could not make $RELEASE_DIR read-only"
fi

# ---------------------------------------------------------------------------
# Atomically swap the current symlink via rename(2).
# The temp symlink lives in the same directory as current so rename(2) is
# guaranteed atomic (same filesystem, same directory).
# ---------------------------------------------------------------------------
LINK_TMP="$RELEASES/.current.new.$$"
ln -s "$RELEASE_NAME" "$LINK_TMP" || {
    printf 'activate: could not create symlink staging file\n' >&2
    exit 1
}
if ! mv -T "$LINK_TMP" "$CURRENT"; then
    rm -f "$LINK_TMP" 2>/dev/null || true
    printf 'activate: could not atomically swap current symlink\n' >&2
    exit 1
fi

log "activate: current -> $RELEASE_NAME"

# ---------------------------------------------------------------------------
# daemon-reload so systemd picks up any unit changes in the new release.
# ---------------------------------------------------------------------------
log "activate: daemon-reload"
"$SC" --user daemon-reload 2>/dev/null \
    || log "activate: WARN: daemon-reload failed — continuing"

# ---------------------------------------------------------------------------
# Restart all active non-aeon spira-* service units.
# ---------------------------------------------------------------------------
_restart_units=()
while IFS= read -r _line; do
    _unit="$(printf '%s\n' "$_line" | awk '{print $1}')"
    [ -n "$_unit" ] || continue
    case "$_unit" in
        spira-aeon-*) continue ;;   # skip: aeons may be live when --force was used
    esac
    _restart_units+=("$_unit")
done < <("$SC" --user list-units --state=active --no-legend 'spira-*.service' 2>/dev/null || true)

if [ "${#_restart_units[@]}" -eq 0 ]; then
    log "activate: no active spira-* service units to restart"
else
    log "activate: restarting: ${_restart_units[*]}"
    "$SC" --user restart "${_restart_units[@]}" 2>/dev/null \
        || log "activate: WARN: some units failed to restart"
fi

# ---------------------------------------------------------------------------
# Prune old releases — best-effort: a failure here must not fail the activation.
# Selection is _prune_candidates (lib.sh): reverse-sorted by name, beyond $KEEP,
# excluding whatever current points at (defence in depth).
# ---------------------------------------------------------------------------
{
    _cur_target="$(readlink "$CURRENT" 2>/dev/null || true)"
    while IFS= read -r _rname; do
        [ -n "$_rname" ] || continue
        log "activate: prune: removing $_rname"
        _rdir="$RELEASES/$_rname"
        chmod -R u+w "$_rdir" 2>/dev/null || true   # un-read-only before removal
        rm -rf "$_rdir"
    done < <(_prune_candidates "$RELEASES" "$KEEP" "$_cur_target")
} || log "activate: WARN: prune encountered an error — release count may exceed $KEEP"

log "activate: done — $RELEASE_NAME is now current"
