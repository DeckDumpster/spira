#!/usr/bin/env bash
set -uo pipefail
bd -C x close y
bdq --db "$DB" update "$1" --status open
bd --actor=me --json reopen "$1"
bd ready --claim --json
bd -C "$DB" --json ready --limit 0
bd -C "$DB" "$verb" x
bd -C x update y --title t
if [ "$(bd -C x show y)" = closed ]; then :; fi
FLAGS=(-C "$DB" --json)
bd "${FLAGS[@]}" claim y
bd --version
