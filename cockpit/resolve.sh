#!/usr/bin/env bash
#
# resolve.sh — close a bead I established or did myself, and do not page the operator about it.
#
#   resolve.sh <bead-id> "<reason>"
#   resolve.sh <bead-id> -            # reason on stdin
#
# WHY (the operator's call): *"for beads that are machine checkable... just go machine
# check them. i don't need to take action if you can. this is perfectly acceptable for
# non-destructive actions."* — law-check-it-yourself-before-asking.
#
# Closing is now routine for me, which created a second-order bug the moment it started:
# watch-answers.sh announced the harness's OWN close back to it as "THE OPERATOR ANSWERED".
# Authorship is recorded — beads writes an audit event of type `closed` carrying the actor,
# which `bd history <id> --events` reads — but it is NOT on the issue row, where there is no
# closed_by, and it is not the Dolt committer, which is the literal string "beads" whatever
# BEADS_ACTOR says. The watcher reads the event. BEADS_ACTOR below is what puts the right
# name on it; the id recorded here is the cheap pre-filter that saves reading the trail.
#
# Reasons carry their evidence. A close that says "done" is a claim with nothing behind it;
# quote the command and its output, so the bead is still readable as a decision months on.
set -uo pipefail

. "$(dirname "$0")/db.sh"
SELF="${SELF_CLOSED:-${SPIRA_SELF_CLOSED}}"

id="${1:-}"; reason="${2:-}"
if [ -z "$id" ] || [ -z "$reason" ]; then
  echo "usage: resolve.sh <bead-id> \"<reason with evidence>\"" >&2; exit 2
fi
[ "$reason" = "-" ] && reason=$(cat)
mkdir -p "$(dirname "$SELF")"

# THE database (db.sh). A close names one database because there is one; the search this
# replaces existed only to pick between copies of the same bead.
db=$(cockpit_db) || exit 1

# Record BEFORE closing: if the close succeeds and the write does not, the watcher pages
# the operator about my own work. The reverse — a recorded id that never closed — is harmless.
printf '%s\n' "$id" >> "$SELF"

# --force because rig beads are usually assigned to the mayor, and closing one as `claude`
# is refused otherwise. The assignee check is right for work; this is a verdict on an ask.
#
# Both streams are merged into one variable because bd exits 0 when it refuses, printing the
# complaint to stdout rather than stderr (the "bd exits 0 when it refuses" paragraph in
# spira.md). Discarding stdout was how the schema-mismatch refusal on 2026-09-08 hid behind
# a generic "failed to close" line instead of naming the database-wide write outage.
bd_out=$(BEADS_ACTOR=claude "$BD" -C "$db" close "$id" --force --reason "$reason" 2>&1)
bd_rc=$?
if [ "$bd_rc" -eq 0 ]; then
  echo "resolved $id (${db##*/})"
else
  printf 'resolve.sh: failed to close %s in %s\n' "$id" "$db" >&2
  printf '%s\n' "$bd_out" | sed 's/^/  bd: /' >&2
  exit 1
fi
