#!/usr/bin/env bash
set -uo pipefail

show_title() {
    bd show "$1" --field title
}

update_assignee() {
    bd update "$1" --assignee "$2"
}
