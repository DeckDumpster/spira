#!/usr/bin/env bash
#
# tsd-ingest.sh — coordinator-side ingest: pull a CI gate run's tsd/*.jsonl rows (uploaded
# in its batch-results artifact, sp-au8a7) into the local run/tsd/ tree, so one family file
# answers queries over local and CI producers together. Every row already carries its own
# producer's host id and clock stamp (law-producers-stamp-their-own-clock); ingest appends
# each line unchanged, it does not relabel it.
#
# usage: tsd-ingest.sh <owner/repo> <run-id>
#
# IDEMPOTENT PER RUN ID. A marker under $SPIRA_RUN/tsd/.ingested-runs/<run-id> is written
# after a successful ingest; a repeat call for the same run id is a no-op. Called from a
# timer over the same recently-completed runs on every tick (gate-check.sh), so this is
# what keeps a run's suite times from being counted twice.
#
# covers: spira/tsd-ingest.sh spira/gate-check.sh spira/tsd-query.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/conf.sh"
. "$HERE/lib.sh"

REPO="${1:?usage: tsd-ingest.sh <owner/repo> <run-id>}"
RUN_ID="${2:?usage: tsd-ingest.sh <owner/repo> <run-id>}"

MARKER_DIR="$SPIRA_RUN/tsd/.ingested-runs"
MARKER="$MARKER_DIR/$RUN_ID"
[ -e "$MARKER" ] && exit 0

spira_require gh unzip flock || exit 1

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

_art_id="$(ghq api "repos/$REPO/actions/runs/$RUN_ID/artifacts" \
    --jq '.artifacts[] | select(.name | startswith("batch-results-")) | .id' 2>/dev/null | head -1)"
if [ -z "$_art_id" ]; then
    printf 'tsd-ingest: no batch-results artifact for run %s\n' "$RUN_ID"
    exit 0
fi

if ! ghq api "repos/$REPO/actions/artifacts/$_art_id/zip" > "$WORK/artifact.zip" 2>/dev/null; then
    printf 'tsd-ingest: could not download artifact %s for run %s\n' "$_art_id" "$RUN_ID" >&2
    exit 1
fi
if ! unzip -q "$WORK/artifact.zip" -d "$WORK/extracted" 2>/dev/null; then
    printf 'tsd-ingest: artifact %s for run %s did not unzip\n' "$_art_id" "$RUN_ID" >&2
    exit 1
fi

_n_families=0
while IFS= read -r -d '' src; do
    family="$(basename "$src" .jsonl)"
    dest="$SPIRA_RUN/tsd/$family.jsonl"
    mkdir -p "$(dirname "$dest")"
    # Append-only merge under an exclusive lock, same discipline tsd-write's IO seam uses —
    # skip a line already present so a re-ingested artifact (retried timer tick before the
    # marker above lands) cannot duplicate rows.
    (
        exec 200>"$dest.lock"
        flock -x 200
        _tmp_seen="$WORK/seen.$family"
        [ -f "$dest" ] && cp "$dest" "$_tmp_seen" || : > "$_tmp_seen"
        while IFS= read -r _line; do
            [ -n "$_line" ] || continue
            grep -qxF "$_line" "$_tmp_seen" 2>/dev/null && continue
            printf '%s\n' "$_line" >> "$dest"
            printf '%s\n' "$_line" >> "$_tmp_seen"
        done < "$src"
    )
    _n_families=$((_n_families + 1))
done < <(find "$WORK/extracted" -path '*/tsd/*.jsonl' -print0 2>/dev/null)

mkdir -p "$MARKER_DIR"
: > "$MARKER"
printf 'tsd-ingest: run %s — ingested %d family file(s)\n' "$RUN_ID" "$_n_families"
