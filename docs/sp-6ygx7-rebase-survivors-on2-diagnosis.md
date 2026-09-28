# sp-6ygx7: landing pass rebase_survivors() is O(n^2) per pass

Diagnosis confirmed by source read (not yet live-profiled). See bead sp-6ygx7 for
full write-up and sp-4hs0i for the tracked fix.

**Mechanism:** `land_repo()` calls `rebase_survivors "$repo" "$name" "$base" "$br"
"${!judged[@]}"` after EVERY successful push-mode landing, sweeping the full
remaining judged-survivor set each time. Because the base just advanced, almost no
survivor passes the cheap merge-base early-continue, so each sweep pays
`content_landed` + `rebase_branch` (real checkout/replay) for nearly every
remaining survivor. Across a pass landing k of n open branches this is ~k*n
rebase-equivalent work, not O(n). Matches the sp-uk1gc cliff (40->80 branches:
72s->312s already superlinear; 80->90, +12% count: 312s->~3000s, a 10x cliff).
Only push-mode repos reach this path — PR-mode branches return early.

**SOP:** sop-landing-pass-rebase-survivors-on2 (this session, sp-6ygx7). Note
`sop.sh write` updates the canonical copy of
`wiki/notes/standard-operating-procedures.md` in the brain repo
(`/home/ryan/spira/brain`), which is outside this worktree and was not committed
here — that repo's own process owns its history. This file exists so the
diagnosis has a citable artifact inside the spira worktree/commit history too.

**Fix status:** not implemented. Tracked as sp-4hs0i (coalesce the sweep to run
once at the end of the pass, or memoize per-survivor "current as of base
generation G", or batch rebase_branch worktree reuse). A separate per-branch
re-check already exists right before each branch's own landing, so one
base-generation staleness mid-pass between sweeps is expected to be safe, but a
builder must confirm before landing the fix.
