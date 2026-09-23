# Is there an SOP for resealing the CI runner template, and is the reseal still needed?

Spike for sp-7oecu. Researched and written 2026-09-23. Every source is preserved verbatim in
`is-there-an-sop-for-resealing-the-runner-template-evidence/` and cited by file; `00-method.md`
states what could and could not be reached. No proof-of-concept branch: the one thing that
would have settled the state of the template needs credentials this session does not hold, and
that limit is the document's falsifier rather than a gap in it.

## The question

The operator asked the concierge, verbatim:

> at some point you wanted me to reseal the VM template. do you have an SOP i can follow for
> that?

The bead that carries it asks for two things: **produce `sop-template-reseal` on the shelf**,
and **settle first whether the reseal is still needed**, because evidence had been read as
suggesting the template might already be sealed. What counts as an answer is a runbook the
operator can follow at a console, plus a determinate yes or no on whether to follow it now.

## The answer in one paragraph

**The runbook already existed, under a different name, written 2 minutes before the bead was
filed — and the reseal is still owed.** `sop-template-apt-drift` is the reseal procedure
(`01-`), with a nine-step long form at `wiki/notes/reseal-ci-runner-template.md` in the brain
(`08-`). A second SOP must not be written; the literal ask is the wrong thing to build. What
*was* missing is a path from the question to the runbook: measured, only **1 of 6** realistic
phrasings of "reseal the template" reached it through `sop.sh match`, and the operator's own
wording was one of the five misses (`02-`). That is fixed — one `MATCH` amendment, 6 of 6 now,
zero false positives over 3683 beads. And the deeper finding is that on the automated path the
runbook could not be reached **at all**: a drifted template reaches the harness as an
unattributable red, so the queue bisects, requeues, and then ejects innocent beads while every
message says "unreproduced red" (`07-`). Filed as sp-wargl.

## What I found

### 1. The SOP exists and is correct (observed)

`spira/sop.sh list` carried `sop-template-apt-drift` before this spike ran, 232 words, clean
under `sop.sh lint`. Its `SYMPTOM` is the provision refusal; its `FIX` is the reseal: drain,
confirm no clone is live, full-clone the template to a new VMID, run
`sudo bash scripts/template-substrate.sh` inside it, confirm three literal `masked` strings,
poweroff, convert, repoint the org variable `TEMPLATE_VMID`, prove it with one dispatched run.
Full text at `01-`; the long form at `08-`.

Three details in it check out against source, which is worth saying because a runbook that
quotes code is only as good as its quotations:

- The refusal it quotes is real and at `provision.sh:593` on `origin/main` = `487a7d5`; the log
  line its `CHECK` depends on is line 583 (`03-`).
- Its "prints masked three times" step is deliberately **stricter** than
  `template-substrate.sh --check`, which accepts `disabled` as clean while `provision.sh`'s
  post-mask verify demands exactly `masked` (`03-`, `04-`). Trust the SOP's step, not the
  script's exit code.
- Its `CHECK` warns that a clean `guestdiag` line is not evidence. Verified independently
  here: this repository's own `gate.yml` masks all three units in-job at line 173 and prints
  the diagnostic at line 293 (`05-`).

### 2. The reseal is still owed (inferred, and the inference is named)

The evidence that had been read as "maybe already sealed" was a gate run printing
`guestdiag: unattended-upgrades=masked apt-daily=masked apt-daily-upgrade=masked` under a
provision version with no mask block. That reading is a positive-control failure and I
reproduced the refutation from source rather than taking it on trust (`05-`): the gate masks
the units itself, before the diagnostic. The line has zero discriminating power for this
question.

With that removed, what remains: the template was last measured drifted 2026-09-18, no reseal
has been recorded since, and `gate.yml:165` still carries the harness's own comment *"The
runner template ships apt-daily and apt-daily-upgrade enabled"* as the reason its workaround
exists. **So: reseal.** I could not confirm today's state directly — see the falsifier.

### 3. Nothing asked the shelf, and the shelf could not have answered on the automated path

Two delivery paths exist and the question travelled on neither (`06-`):

- **Injection.** Only `ops.fayth` declares `FAYTH_MEMORY_PREFIXES="law-,sop-"`; the concierge,
  where the question was asked, does not receive the shelf — by design. And what Ops receives
  is not the runbooks: `render_memories()` renders full text only for `FAYTH_STATUTE_CORE`
  slugs and Ops declares none, so the shelf arrives as 30 bare slugs, measured at **1127 chars
  total, ~38 per SOP**, under a hardcoded header telling the reader to fetch them with
  `rule.sh show` — which refuses every `sop-` key. Positive control in `06-`. Filed as
  **sp-cvpp0**.
- **`sop.sh match`.** Its only caller in the tree is `spira/chamber/ops.md:12`, fed an
  *incident bead's own text*. Nothing feeds it an operator's prose.

And the automated path cannot produce a matchable payload for this incident at all. Traced end
to end in `07-`: the `gate` job runs on `if: !cancelled()` and exits 75, so a provision failure
concludes FAILURE; `check-status` calls that plain `red`; `forge.sh` extracts annotations only
for paths matching `spira/test-*.sh`, so the provision `::error::` is dropped; `verdict.sh`
sees red with no suite annotations and bisects, requeuing every member; and a single member's
local repro comes back green — nothing local provisions a VM — so it ejects as unreproduced
red. `grep -rn provision spira/*.sh cockpit/*.sh` returns **zero lines** (positive control in
`07-`: the same glob finds `annotation`). Filed as **sp-wargl**, P1.

### 4. A duplicate SOP was the live hazard, not a hypothetical one

The archivist that filed this bead mailed the operator at 04:09Z saying so: *"If the aeon does
not stand down on its own it will write a second SOP for the same symptom, which is worse than
wasted time: two SOPs matching one incident payload make `sop.sh match` ambiguous."* That is
the correct reading and this spike agrees with it on measurement rather than on authority — see
Option B below.

## Options

Costs are measured on this box on 2026-09-23 unless marked as an estimate. Wall-clock figures
for work this spike performed are from its own session; figures for work it did not perform are
estimates from reading the code and are marked.

### Option A — answer with the existing SOP, change nothing

- **Cost: 0 minutes.** The runbook is on the shelf and correct.
- **Risk:** measured 1-in-6. Five of six realistic phrasings of the question — including the
  operator's own — return nothing from `sop.sh match` (`02-`). The next person to ask is
  slightly more likely than not to be told there is no runbook, which is how this bead came to
  exist in the first place.

### Option B — write `sop-template-reseal`, as the bead literally asks

- **Cost: ~30 minutes** to write, plus **~38 characters** in every Ops session prompt (measured:
  1127 chars for 30 slugs, `06-`). The token cost is a rounding error and is *not* the argument
  against this.
- **Risk, and it is the real one: the deterministic tier stops being deterministic.** Two SOPs
  whose `MATCH` fires on the same refusal both score 1 (`sop.sh match` scores by distinct
  payload lines hit, `spira/sop.sh:262-270`), so the output is two equal hits and the reader
  chooses. Worse, they then drift: today exactly one place records that the reseal is not
  finished until the org variable `TEMPLATE_VMID` is repointed. A duplicate that omits it sends
  the operator through a console procedure that changes nothing observable.
- **Verdict: do not.** The bead's literal ask is wrong because the thing it asks for already
  exists under another name.

### Option C — amend the existing SOP's `MATCH` so the prose phrasings reach it

- **Cost: 4 minutes** of session time, one `sop.sh write` upsert, no new shelf entry (30 SOPs
  before and after). The amended SOP is **246 words against the 250-word cap**, so the next
  amendment has four words of headroom and must trim.
- **Measured effect:** prose phrasings 1/6 → 6/6 (9/9 including the refusal text and two
  written forms); **0 false positives over the 3683-bead corpus**; old-regex hits 5, new 7,
  none lost, both additions genuinely on-subject (`02-`).
- **Risk:** a wider regex can fire on unrelated payloads. Controlled for: the obvious widening
  — the bare word `reseal` — is *wrong*, because `reseal` is already the merge queue's verb
  (`_batch_reseal`, "head resealed"), and 4 of the 11 beads containing the word are queue
  beads. The regex adopted requires template context within 0-3 words and is clear on all six
  queue-vocabulary negatives in both regex engines (`02-`).
- **Residual:** this makes the tier answer correctly *when asked*. Nothing in the harness asks
  with prose. So the honest value is narrow: it helps an agent that thinks to pipe the question
  to `sop.sh match`, and it costs almost nothing.

### Option D — fix the two delivery defects

- **sp-cvpp0** (P2): the index tier's retrieval header is composed from one prefix list but
  serves two namespaces. **Estimate: 1-2 hours** — render a per-prefix retrieval line, plus a
  suite that plants a `sop-` key and asserts the rendered command actually returns it.
- **sp-wargl** (P1): give `check-status` a third outcome for "the gate job never tested the
  branch", so `verdict.sh` treats a provision failure as infrastructure rather than as a
  member's red. **Estimate: half a day.** Cost of *not* doing it, measured against the code
  path rather than a log: every future drift silently ejects beads whose code was never run,
  and the runbook naming the cause stays unreachable.
- **Risk:** both are harness changes that touch the queue's hot path; sp-wargl in particular
  changes what a red batch means and wants its own suite before it lands.

## Recommendation

**C, done in this session; B refused; D filed for the loop; and the reseal itself escalated to
the operator, because it is console work no aeon can do.**

Concretely, and in the order it matters:

1. **Answer the operator with `sop.sh show template-apt-drift` and the long form at
   `08-`.** There is a runbook. It was there when he asked; the failure was in reaching it.
2. **The reseal is still owed.** Follow the FIX. Do `PVE_CA_CERT` in the same sitting — the
   long form explains why teardown fails without it and leaks one VM per run.
3. **sp-wargl is the finding worth more than this bead.** A drifted template currently ejects
   innocent beads and blames them.

**The one load-bearing assumption:** that resealing after drift and resealing as planned
maintenance are *the same procedure*, so one runbook serves both. Everything above follows from
it — it is why a second SOP is a duplicate rather than a sibling, and it is why the amendment
was a `MATCH` widening rather than a new entry. It holds today because the FIX's steps do not
branch on *why* you are resealing: clone, run the substrate script, verify, convert, repoint.

## The falsifier — what would have to be true for this to be wrong

Three things, each checkable:

1. **The template is in fact already sealed.** Then recommendation 2 is wasted console work
   (though not harmful — the procedure is idempotent and produces a fresh template either way).
   **Check:** find a provision log from a run after 2026-09-23 03:58Z; the line
   `provision.sh: masking apt automation in guest N` must be **present** — its absence means
   the action predates the check and proves nothing — and
   `::error::… has apt automation enabled` must be **absent**. Or, at the console,
   `bash scripts/template-substrate.sh --check` inside the template. This spike could not run
   either: no Proxmox credentials, and an aeon has no forge read path (`00-`). **This is the
   one claim in the document that rests on inference.**
2. **The two reseals diverge.** If a planned reseal ever needs steps a drift repair does not —
   a different base image, a version pin, a migration — then the assumption above has stopped
   holding and the right answer flips to Option B: split into `sop-template-reseal` (planned)
   and `sop-template-apt-drift` (repair), with the second referring to the first for the
   procedure. **Check:** the moment `FIX` needs an "if you are here because…" branch, split it.
   The 250-word cap will force the question anyway: the amended SOP is at 246.
3. **Something does feed prose to `sop.sh match`.** If a caller is added that pipes operator
   questions to the shelf, Option C stops being cheap insurance and becomes the main path, and
   the `MATCH` lines across all 30 SOPs — most written against log text — become worth
   auditing as a set rather than one at a time. **Check:**
   `grep -rn "sop.sh match" --include='*' .` returning a caller that is not
   `spira/chamber/ops.md`.

## What I could not establish

- **The template's current state.** Named above as falsifier 1. Everything else in this document
  is observed or read from source.
- **Whether the `v1` tag actually moved to `487a7d5` at 03:58Z.** `origin/main` is at `487a7d5`
  (observed); the local `v1` still resolves to `ae8995d` and cannot be refreshed without a
  fetch (`git fetch` fails: *"aeon: no SSH credentials"*). The tag advance is taken from the SOP
  and the long form, unchecked.
- **Whether destroying the old template is safe while linked clones exist.** The long form's
  step 9 says to destroy it once a batch lands green on the new one; clones are created with
  `--full 0` and share the template's base disk (`04-`). Proxmox is believed to refuse the
  delete while linked clones exist, which would make the step self-protecting — **not verified,
  and worth one console check before anyone runs step 9.**

## Filed, not fixed

| Bead | P | What |
| --- | --- | --- |
| sp-wargl | P1 | A provision-stage failure reaches the harness as an unattributable red; the queue bisects then ejects innocent beads as unreproduced-red, and no payload names the cause |
| sp-cvpp0 | P2 | Ops receives the shelf as bare slugs under an instruction (`rule.sh show`) that refuses every `sop-` key |
| sp-9qa7o | P3 | `sop.sh`'s word-cap refusal states a cost the code stopped charging: the shelf reaches Ops as slugs, not as 250-word bodies |

## Changed on the shelf, not in this repository

`sop-template-apt-drift` was amended in place through `spira/sop.sh write` — a database write,
plus the regenerated page `synth` renders into the brain. Nothing in this repository changed
except this document and its evidence directory. Diff of the stored SOP text is in `01-`.
