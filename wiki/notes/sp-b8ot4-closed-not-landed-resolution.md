# sp-b8ot4: SYSTEMIC closed-not-landed — resolution case history

Incident: closed beads without LANDED records; root cause was `bead_close_on_land`
(lib.sh) not calling `land_mark LANDED` after a successful close.

## Timeline
- Root cause fix **sp-2dvyh** landed at commit `6221cc93c`.
- `sop-closed-not-landed-false-alarm` was applied to sp-b8ot4 (2026-09-27,
  aeon-sandy): held=yes. CHECK confirmed most named beads (sp-1itjj, sp-vb05n,
  sp-g44ke) ARE on origin/main despite missing LANDED records — classic false
  alarm, fixed at the source going forward.
- Exception at the time: `sp-zs04v.4` was genuinely not landed (OOM killed
  the landing gate mid-run).
- By the time this session picked the bead back up, both children this
  incident BLOCKS — **sp-zfkoc** (for sp-zs04v.4) and **sp-lvw4e** (for
  sp-g44ke) — had independently closed. `sp-zs04v.4` itself was reopened as a
  fresh OPEN task (not retroactively patched) and is being reworked normally.
- No separate "retroactive LANDED record creation" repair step ever ran, and
  none was needed: individual closed-not-landed incidents clear on their own
  once the underlying bead either lands for real or gets requeued.

## Conclusion
Once sp-2dvyh is landed AND an incident's own BLOCKS list is all CLOSED, the
systemic incident is resolved. New closed-not-landed incidents for *other*
beads (there were 9+ in-flight at close time, e.g. sp-anw3d, sp-qzqua,
sp-onts9, sp-yzddf, sp-2ckly) are separate, ordinary occurrences of the same
already-fixed-going-forward bug working through the backlog — not a reason to
keep this incident open.
