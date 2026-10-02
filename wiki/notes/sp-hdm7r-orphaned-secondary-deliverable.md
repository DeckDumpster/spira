---
type: note
created: 2026-09-29
tags: [spira, ops, sop, incident, sp-hdm7r]
---

# sp-hdm7r: orphaned secondary deliverable in a closed+superseded bead

sp-r66qd closed 2026-09-28, superseded by sp-i2m7y. The successor's commit
(38580739c) IS an ancestor of local/main, so the primary fix (lib.sh)
genuinely landed and `bd supersede` correctly recorded that.

But sp-r66qd's own close reason made a second claim: "what still lands from
this branch: the regression test (test-attempts-sql.sh, case c5)". Supersede
only checks the successor's commit, so it certified the bead as retired
without anyone checking the second promise. Verified:

    git merge-base --is-ancestor f19c7e6b5 local/main   # rc=1, not landed
    git diff local/main spira/sp-r66qd -- spira/test-attempts-sql.sh
    # case c5 present only on the closed branch

None of the existing shapes in sop-closed-not-landed-false-alarm fit: shape 1
needs the *referenced* commit to not be an ancestor (the successor's is),
shape 2 needs the bead OPEN (it's closed), shape 3 needs the close reason to
cite direct ops action (this one cites a supersede). This is shape 4, added
to that SOP.

Fix taken: filed sp-93vap carrying the orphaned commit's diff inline (16
lines, self-contained) so it lands through the normal queue/gate path.
Did not reopen sp-r66qd or make sp-hdm7r depend on sp-93vap — sp-hdm7r's own
deliverable (the diagnosis) is complete once filed.

Note: sp-5ayw5 and sp-7qk8u are open beads already touching
spira/test-attempts-sql.sh — whoever picks up sp-93vap should rebase against
whichever of those lands first.
