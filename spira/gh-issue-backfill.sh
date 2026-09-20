#!/usr/bin/env bash
#
# gh-issue-backfill.sh — apply gh_issue_closeout to existing closed+landed beads.
#
#   gh-issue-backfill.sh [--dry-run]
#
# For each closed bead with a github: external_ref, locate its landing commit by
# ancestry (git log --grep on the land ref, falling back to the landstate tip when
# it is itself an ancestor). Beads whose commit is not on any land ref are reported
# to the operator rather than skipped silently.
#
# Run --dry-run first; the concierge supervises the first real pass
# (law-one-supervised-pass-before-you-arm).
#
# covers: spira/gh-issue-backfill.sh spira/lib.sh spira/landing.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/lib.sh"

DRY=0
while [ $# -gt 0 ]; do
    case "$1" in
        --dry-run) DRY=1; shift ;;
        -h|--help) sed -n '2,5p' "$0"; exit 0 ;;
        *) printf 'gh-issue-backfill: unknown argument: %s\n' "$1" >&2; exit 2 ;;
    esac
done

n_found=0; n_closed=0; n_skipped=0; n_dry=0

_tmp="$(mktemp)"
trap 'rm -f "$_tmp"' EXIT INT TERM

bdjson list --all --limit 0 2>/dev/null | python3 -c '
import sys, json
try: d = json.load(sys.stdin)
except Exception: raise SystemExit(0)
rows = d if isinstance(d, list) else [d]
for r in rows:
    if r.get("status") != "closed": continue
    ext = r.get("external_ref") or ""
    if not ext.startswith("github:"): continue
    bid = r.get("id", "")
    if not bid: continue
    print(bid + "\t" + ext)
' 2>/dev/null > "$_tmp"

while IFS=$'\t' read -r _id _ext; do
    [ -n "$_id" ] || continue
    n_found=$(( n_found + 1 ))

    if [ -e "${SPIRA_RUN}/gh-closed/$_id" ]; then
        log "gh-issue-backfill: $_id: already closed — skipping"
        n_skipped=$(( n_skipped + 1 ))
        continue
    fi

    # Read landstate sha for fallback; do not trust the state field (may be stale).
    _ls_sha=""
    _ls_file="$SPIRA_RUN/landstate/$_id"
    [ -r "$_ls_file" ] && { read -r _ _ls_sha _ < "$_ls_file" 2>/dev/null || true; }

    # Find the landing commit by ancestry: git log --grep for the bead id on the
    # land ref, then the landstate tip if it is itself an ancestor.
    _landed_sha=""
    _repo_path=""
    for _rn in $(spira_repos 2>/dev/null); do
        _rp="$(repo_root "$_rn" 2>/dev/null)" || continue
        _lref="$(spira_landref "$_rn" 2>/dev/null)" || continue
        _c="$(git -C "$_rp" log --format='%H' --grep="$_id" "$_lref" 2>/dev/null | head -1)" \
            || _c=""
        if [ -n "$_c" ]; then
            _landed_sha="$_c"; _repo_path="$_rp"; break
        fi
        if [ -n "${_ls_sha:-}" ] \
           && git -C "$_rp" cat-file -e "$_ls_sha" 2>/dev/null \
           && git -C "$_rp" merge-base --is-ancestor "$_ls_sha" "$_lref" 2>/dev/null; then
            _landed_sha="$_ls_sha"; _repo_path="$_rp"; break
        fi
    done

    if [ -z "$_landed_sha" ]; then
        if [ "$DRY" = 1 ]; then
            log "gh-issue-backfill: $_id: no commit on land ref — would ask operator"
        else
            log "gh-issue-backfill: $_id: no commit on land ref — asking operator"
            gh_issue_ask_unlanded "$_id" "$_ext" || true
        fi
        n_skipped=$(( n_skipped + 1 ))
        continue
    fi

    if [ "$DRY" = 1 ]; then
        printf 'would close %s (%s) as %s\n' "$_ext" "$_id" "${_landed_sha:0:8}"
        n_dry=$(( n_dry + 1 ))
        continue
    fi

    gh_issue_closeout "$_id" "$_landed_sha" "$_repo_path"
    _rc=$?
    if [ "$_rc" -eq 0 ] && [ -e "${SPIRA_RUN}/gh-closed/$_id" ]; then
        n_closed=$(( n_closed + 1 ))
    else
        n_skipped=$(( n_skipped + 1 ))
    fi
done < "$_tmp"

if [ "$DRY" = 1 ]; then
    printf 'gh-issue-backfill: dry run — found %d, would close %d, skip %d\n' \
        "$n_found" "$n_dry" "$(( n_found - n_dry ))"
else
    printf 'gh-issue-backfill: found %d, closed %d, skipped %d\n' \
        "$n_found" "$n_closed" "$n_skipped"
fi
