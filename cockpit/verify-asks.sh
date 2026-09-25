#!/usr/bin/env bash
#
# verify-asks.sh — close the asks the operator has already satisfied, before they have to say so.
#
#   verify-asks.sh            # check and report; changes nothing
#   verify-asks.sh --apply    # close the ones whose own check now passes
#
# WHY, in the operator's own words on closing one: *"ran those commands to verify, but i already
# did this days ago. close it out."*
#
# That bead asked them to subscribe a calendar feed. It sat open for three days, and it
# carried the exact command that proves the work is done — `cal source list` showing one
# source. Nobody ran it. Run today it exits 0 and prints the source: the ask had been
# satisfied the whole time and was still sitting in their queue with their name on it.
#
# CLAUDE.md already said to prefer a machine check over asking, and cited three of nineteen
# items provable from systemd and the commit graph. That was advice, and advice is a thing to
# remember. This runs the check.
#
# THE CONVENTION. An operator ask carries a line in its description:
#
#     VERIFY: <shell command>
#
# exiting 0 when the ask is ALREADY SATISFIED. Keep it read-only — a listing, a `systemctl
# is-active`, a `git merge-base --is-ancestor`, a `test -f`. It runs unattended and it runs
# repeatedly, so anything with a side effect is a bug in the bead, not a clever shortcut.
#
# Closing carries the command's own output as evidence, because a bare tick is a claim with
# nothing behind it — so prefer a check that PRINTS what it found over one that is silent.
# `grep -q` closes the bead with empty evidence; `grep` without -q closes it with the
# matching line, which is the thing a human would actually want to see.
#
# THE beads database, via db.sh. This walked the town alone for a time, which meant
# sp-builder-cutover — Spira's first escalation, and one that carries a VERIFY line — had
# nothing that would ever run it.
set -uo pipefail

. "$(dirname "$0")/db.sh"
apply=""; [ "${1:-}" = "--apply" ] && apply=1

# ONE fetch, two passes over the same rows: fetching again inside each loop would double the
# cost of a check that runs unattended on a timer.
db=$(cockpit_db) || {
  echo "  verify-asks: no beads database — checked nothing"
  exit 0
}
rows=$(cockpit_beads) || {
  echo "  verify-asks: the beads database is not reachable — checked nothing"
  exit 0
}

found=0; closed=0
# id<TAB>command, one per open escalated bead that declares a check.
while IFS=$'\t' read -r id cmd; do
  [ -n "$id" ] || continue
  found=$((found+1))
  out=$(timeout "${SPIRA_VERIFY_TIMEOUT:-120}" bash -c "$cmd" 2>&1); rc=$?
  short=$(printf '%s' "$out" | head -c 400)
  if [ "$rc" -eq 0 ]; then
    echo "  SATISFIED  $id  ($cmd)"
    if [ -n "$apply" ]; then
      # `--force`: an ask decomposed out of an epic inherits its `blocks` edges, and a
      # blocked close is refused. The evidence here is the ask's OWN check passing, which is
      # a stronger statement than the dependency graph's guess about ordering.
      BEADS_ACTOR=claude "$BD" -C "$db" close "$id" --force \
        --reason "Verified already done — its own VERIFY check now passes: ${cmd} → exit 0. Evidence: ${short}" \
        >/dev/null 2>&1 && { echo "    closed"; closed=$((closed+1)); }
    fi
  else
    echo "  still open $id  (check exit $rc)"
  fi
done < <(printf '%s' "$rows" | python3 -c '
import json, os, re, sys
ASK = os.environ.get("SPIRA_ASK_LABEL", "needs-operator")  # literal-ok: Python fallback for direct invocation without conf.sh
try:
    doc = json.load(sys.stdin)
except Exception:
    sys.exit(0)
rows = doc if isinstance(doc, list) else doc.get("issues", [])
for r in rows:
    labels = r.get("labels") or []
    if ASK not in labels:
        continue
    if (r.get("status") or "") == "closed":
        continue
    m = re.search(r"^\s*VERIFY:\s*(.+)$", r.get("description") or "", re.M)
    if m:
        print("%s\t%s" % (r.get("id"), m.group(1).strip()))
')

# STRUCTURALLY UN-ANSWERABLE ASKS. An epic is a container for a branch of work; it cannot be
# a decision however it is labelled. Two reached the operator carrying the escalation label from a
# deferred-sweep triage, holding nothing but two lines of branch metadata, and they closed both
# asking "what is the decision?" The pane no longer shows them — but hiding one would leave
# the label wrong and the next tool to read it confused, so they are named here instead.
bogus=0
while IFS=$'\t' read -r id kind title; do
  [ -n "$id" ] || continue
  bogus=$((bogus+1))
  echo "  MISLABELLED $id ($kind) — $title"
  echo "              an ${kind} cannot be answered; strip $SPIRA_ASK_LABEL or write the question"
done < <(printf '%s' "$rows" | python3 -c '
import json, os, sys
ASK = os.environ.get("SPIRA_ASK_LABEL", "needs-operator")  # literal-ok: Python fallback for direct invocation without conf.sh
try:
    doc = json.load(sys.stdin)
except Exception:
    sys.exit(0)
rows = doc if isinstance(doc, list) else doc.get("issues", [])
for r in rows:
    if ASK not in (r.get("labels") or []):
        continue
    if (r.get("status") or "") == "closed":
        continue
    if r.get("issue_type") == "epic":
        print("%s\t%s\t%s" % (r.get("id"), r.get("issue_type"), (r.get("title") or "")[:60]))
')
[ "$bogus" -gt 0 ] && echo "  ${bogus} ask(s) cannot be answered by anyone — fix the label, not the pane"

echo "  ${found} ask(s) carry a check; ${closed} closed"
[ "$found" -eq 0 ] && echo "  (none yet — add a 'VERIFY: <cmd>' line to operator asks)"
exit 0
