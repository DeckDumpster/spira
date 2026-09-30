#!/usr/bin/env bash
# panel-run.sh — launch the attention panel with configuration resolved, every time.
#
# WHY A LAUNCHER AND NOT THE BINARY DIRECTLY. tmux gives a new pane the environment of the
# tmux SERVER, not of the process that ran `split-window`. layout.sh sources conf.sh and so
# knows COCKPIT_DB, SPIRA_ASK_LABEL and where bd lives — and none of that reached the pane.
# The panel reads COCKPIT_DB (falling back to SPIRA_DB) with no default, deliberately, since
# a panel that guessed a database would read someone else's conversation; unset, it ran
# `bd -C ""`, which resolves from the pane's cwd and found no database there at all.
#
# It was worse than a visible error, too: SPIRA_ASK_LABEL is what tells the panel which label
# means "waiting on the operator". Unset it falls back to a default the installation does not
# use, matches nothing, and shows an EMPTY list rather than an error — the failure that looks
# like good news (law-absence-needs-a-positive-control).
#
# Sourcing at launch rather than exporting into the tmux server is the point: the server's
# environment is set once and then goes stale, while this re-reads spira.conf on every respawn,
# so a config change takes effect on the next restart with nothing to remember.
#
# `exec` matters: the pane's running process must remain the binary, since the @cockpit tag
# is derived from /proc rather than from the command the pane was created with. The panel is
# invoked by bare name on the launcher's PATH (sp-gypjk); a release always carries it, so a
# missing one fails here, naming itself.
set -uo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]}")/../spira" && pwd -P)/conf.sh"

exec panel "$@"
