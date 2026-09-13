Groom pass sp-8hcu: Execute Ryan's 2026-09-13 queue purge

## Verified work beads (closed per verdict)

* sp-qd21 — [CLOSED] P0, aeon slain
* sp-4u2n — [CLOSED] P1, completed (slay.sh commits on origin/main)
* sp-bsl9 — [CLOSED] P3, test machinery
* sp-a6t9 — [CLOSED] P2, test machinery
* sp-k5tp — [CLOSED] P2, test machinery
* sp-u06o — [CLOSED] P1, test machinery

All six beads carry close reasons: "Dropped on Ryan's verdict 
2026-09-13 (turn 642): scope narrowed to release unit (sp-3hw3), 
test selection (sp-hqo1), and harness self-repair only."

## Special cases

* sp-mzbk — [ESCALATED] Audit of newly-enacted law 
  (law-tools-take-named-arguments). Filed as ask sp-o36s to resolve 
  whether to keep the audit open or close it (decision depends on whether 
  gate will enforce the law).

* sp-9edh — [NOTED] Live aeon already completed and closed with evidence 
  (commit 8ff9b4f). Harness self-repair, kept per verdict.

* sp-6bop, sp-0wqx — [NOTED] Harness self-repair beads, IN_PROGRESS, kept 
  per verdict.

## Queue status

Ready queue reports 16 beads. Expected composition (only release unit, 
test selection, harness self-repair) appears incomplete — several beads 
in ready queue do not appear to belong to those categories. Queued for 
yard-level review of scope boundaries.

---
Groom pass complete. All findings filed on sp-8hcu before close.
