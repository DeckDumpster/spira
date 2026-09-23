# 06 — how a runbook actually reaches a reader, measured

Observed 2026-09-23 in this worktree. Two delivery paths exist, and the SOP shelf is on neither
of the ones the operator used.

## Path 1 — injection into a session, and what Ops really receives

```
$ grep -n FAYTH_MEMORY_PREFIXES spira/chamber/*.fayth
spira/chamber/builder.fayth:27:FAYTH_MEMORY_PREFIXES="law-"
spira/chamber/concierge.fayth:54:FAYTH_MEMORY_PREFIXES="law-"
spira/chamber/czar.fayth:28:FAYTH_MEMORY_PREFIXES="law-"
spira/chamber/groomer.fayth:19:FAYTH_MEMORY_PREFIXES="law-"
spira/chamber/maechen.fayth:20:FAYTH_MEMORY_PREFIXES="law-"
spira/chamber/ops.fayth:32:FAYTH_MEMORY_PREFIXES="law-,sop-"
spira/chamber/spike.fayth:31:FAYTH_MEMORY_PREFIXES="law-"
```

Only Ops gets the shelf. The concierge — where the question was asked — does not, by design
(`spira/aeon.sh:1587-1591`: *"SOPs are how to fix and only Ops executes one"*).

But `render_memories()` (`spira/lib.sh:3170`) renders **full text only for slugs named in
`FAYTH_STATUTE_CORE`**; everything else becomes a bare slug in an index tier. `ops.fayth`
declares no `FAYTH_STATUTE_CORE`. Measured:

```
$ . spira/lib.sh; render_memories "sop-" "" "" | wc -c -w -l
sop block: 1127 chars, 70 words, 36 lines      # for 30 SOPs — ~38 chars each
law block: 12361 chars, 1331 words
$ ./spira/sop.sh list | <sum the word counts>
3531                                            # the same 30 SOPs as text
```

So an Ops session pays about **38 characters per SOP**, not 118 words. Two consequences:

1. The `sop.sh` word-cap refusal — *"Every Ops session pays for every SOP"* — no longer
   describes the mechanism. Filed as **sp-9qa7o** (P3).
2. The index tier's header is hardcoded for statutes and names a retrieval command that cannot
   fetch an SOP. With a positive control:

```
$ ./rule.sh show template-apt-drift
rule: no statute 'law-template-apt-drift' — `rule.sh list` shows what is in force
rc=1

$ ./rule.sh show claim-before-you-work          # positive control, a law- slug
You may not touch a bead's branch, worktree, labels or status unless you hold its claim …
rc=0
```

`rule.sh` prepends `law-`. So Ops receives 30 `sop-*` slugs under the instruction
*"read the reasoning and the scar behind any of them with `rule.sh show <slug-without-law-prefix>`"*,
which refuses every one. `spira/chamber/ops.md:12` separately gives the right command
(`sop.sh show <slug>`), so both texts are in context and they disagree. Filed as **sp-cvpp0**
(P2).

## Path 2 — `sop.sh match`, and what is fed to it

`spira/chamber/ops.md:1-14` is the only caller in the tree:

```
bd -C {{DB}} show {{BEAD_ID}} > /tmp/{{BEAD_ID}}.payload
{{SOP}} match /tmp/{{BEAD_ID}}.payload
```

The payload is an **incident bead's own text**. Nothing feeds an operator's prose question to
it, and the concierge's tool list (`spira/chamber/concierge.md:94`) offers only
`sop.sh write|applied` — not `show`, `list` or `match`.

```
$ grep -rn "sop\.sh" --include='*' . | grep -v './spira/sop.sh:\|./spira/test-'
   … no caller of `sop.sh match` outside spira/chamber/ops.md …
```

So the runbook was on the shelf and correct, and the path from *"do you have an SOP for that?"*
to it ran through an agent choosing to run `sop.sh list` on a hunch. Widening `MATCH` (see
`02-`) makes the deterministic tier answer that phrasing; it does not make anything ask.
