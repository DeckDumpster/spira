# db.sh — the beads database. Singular. Sourced, never executed.
#
# (the operator, verbatim: "i don't think we need to keep the databases in sync here. let's
# just use the spira one for everything from here on out. i don't want this
# migration/deprecation to bake a bunch of complexity and modality into our tools.")
#
# Everything the cockpit raises, reads, answers and closes lives in Spira. There is no walk,
# no precedence order, no per-row `_db` tag and no dedupe, because all four existed for one
# reason — two live databases holding the same beads — and that reason is gone.
#
# They were also a bug factory in their own right, which is why they are deleted rather than
# configured down to one entry. A reply answered in the town left Spira's replica still
# reporting the thread unanswered; a dedupe recorded only the ids that complained, so an
# ANSWERED bead in the first database let a stale replica speak for it; and the shell tools
# were switched to Spira while the panel was not, splitting one conversation across two
# databases within the hour — their 18:54 comment landed in the town because the pane wrote
# there, while unanswered.sh read Spira and reported nothing waiting.
#
# Gas Town's own databases are not addressed from here at all, and nothing else addresses
# them either: `rule.sh` writes the statute book to this same database, and it is the one
# every aeon reads its law from at summon.
# Every path comes from the harness's one configuration surface. It is two directories
# away because the cockpit ships beside the harness, not inside it.
. "$(cd "$(dirname "${BASH_SOURCE[0]}")/../spira" && pwd -P)/conf.sh"
# conf.sh has already resolved COCKPIT_DB, defaulting it to SPIRA_DB. This line is the
# guard for a db.sh sourced with neither set.
COCKPIT_DB="${COCKPIT_DB:-${SPIRA_DB:-}}"
# Bead content is read and written through `spira-lc content`, never `bd`; SPIRA_LC_BIN stays
# the seam a suite drives a stub through.
LC="${SPIRA_LC_BIN:-spira-lc}"

# The database, verified to be one. A missing `.beads` is a real fault to report, never a
# reason to quietly address some other database instead.
cockpit_db() {
  [ -d "$COCKPIT_DB/.beads" ] && { printf '%s\n' "$COCKPIT_DB"; return 0; }
  echo "cockpit: $COCKPIT_DB has no .beads — refusing to guess a database" >&2
  return 1
}

# Every bead in it, as one JSON array on stdout. Exits 1 if it could not be read, so a caller
# can hold its previous output rather than render "nothing is waiting" — a panel that reports
# a broken check as all-clear displaces the suspicion that would have prompted a look
# (law-alerts-must-be-actionable).
#
# One function rather than the same three lines in each of five scripts: that duplication is
# exactly how the cockpit ended up with five copies of a database walk, four of which were
# never corrected when the fifth was. `--json` can print warnings on stdout BEFORE the
# payload, which is what the sed strips.
cockpit_beads() {
  local db out
  db=$(cockpit_db) || return 1
  out=$("$LC" content list --all --limit 0 --json 2>/dev/null | sed -n '/^[[{]/,$p')
  [ -n "$out" ] || return 1
  printf '%s' "$out"
}

# Only the beads the attention surface is ABOUT, narrowed by the server rather than by the
# reader. `--label-any` is an OR: the escalation label and `overseer` for a question,
# `insight` for an FYI, which is created closed and carries neither.
#
# WHY IT IS A SEPARATE FUNCTION. `cockpit_beads` promises every bead and something may yet
# want that; this promises a subset and says which. Measured on a live database: 1867 rows in
# 644ms against 109 rows in 342ms, every 45 seconds, forever — and the whole difference was
# being thrown away one line later.
#
# A NARROWED QUERY IS A PLACE TO GO BLIND, which is why the label comes from configuration
# and never from a literal: an installation whose escalation label is its own would otherwise
# match nothing and report, in perfect silence, that nobody has answered anything
# (law-absence-needs-a-positive-control). spira/test-cockpit-db.sh pins it to a non-default and
# asserts the count, so a literal written back in here fails the gate.
cockpit_attention_beads() {
  local db out
  db=$(cockpit_db) || return 1
  out=$("$LC" content list --all --limit 0 \
        --label-any "insight,$SPIRA_ASK_LABEL,overseer" --json 2>/dev/null \
        | sed -n '/^[[{]/,$p')
  [ -n "$out" ] || return 1
  printf '%s' "$out"
}
