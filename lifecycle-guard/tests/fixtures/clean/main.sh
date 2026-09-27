#!/usr/bin/env bash
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/lib.sh"

title="$(show_title sp-1)"
echo "title is $title"
update_assignee sp-1 ""
