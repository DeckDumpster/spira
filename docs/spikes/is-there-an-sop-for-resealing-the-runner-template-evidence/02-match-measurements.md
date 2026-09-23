# 02 — the discoverability measurement, with both controls

Observed 2026-09-23. `sop.sh match` is the deterministic tier: it compiles each SOP's `MATCH`
line as a Python regex (case-insensitive, multiline) and scores by distinct payload lines hit
(`spira/sop.sh:256-285`). `sop.sh write` separately requires the same string to compile under
`grep -E` (`spira/sop.sh:199-209`), so a `MATCH` must be valid in **both** engines.

## Before: one of six realistic phrasings hit

The operator's own words are the first row, taken verbatim from the question that produced the
bead.

```
$ for p in …; do printf '%s\n' "$p" | ./spira/sop.sh match - ; done

reseal the VM template                                  =>
reseal the template                                     => sop-template-apt-drift
how do I reseal template 9110                           =>
template reseal                                         =>
resealing the runner template                           =>
do you have an SOP for resealing the VM template        =>
```

One hit in six. The old regex carried the literal `reseal the template`, so "the **VM**
template" — one inserted word — missed. The refusal text itself matched, as designed:

```
$ printf '::error::provision.sh: template 9110 has apt automation enabled; reseal the template (bash scripts/template-substrate.sh) before provisioning\n' | ./spira/sop.sh match -
sop-template-apt-drift	MATCH	1	Provision refuses with "::error::provision.sh: …
```

## The negative control that killed the obvious fix

The obvious widening is the bare word `reseal`. It is wrong, and the harness says so: `reseal`
is already a load-bearing verb in the **merge queue**, unrelated to any template.

```
$ grep -in "reseal" spira/verdict.sh spira/test-attribution.sh spira/chamber/czar.md
spira/verdict.sh:34:_batch_reseal() {   # _batch_reseal <file> <new_head> <new_members>
spira/verdict.sh:564:                        _batch_reseal "$batch_file" "$_new_head" "${survivors[*]}"
spira/test-attribution.sh:311:#    require BATCHED state, a resealed batch file, and the remote branch updated.
spira/test-attribution.sh:333:is   "1. breaker: head resealed"                  "$_new_head1" "$_remote_head1"
spira/test-attribution.sh:499:is   "6. diff-attr: head resealed"                           "$_new_head6" "$_remote_head6"
spira/chamber/czar.md:101:survivors in the same PR number (head= resealed). Reopen the ejected bead with the
```

Four of the eleven beads whose text contains `reseal` are queue-attribution beads, not template
beads — e.g. `sp-x16x3` ("hand (head resealed to 34ff596)") and `sp-djvo1` ("halved: reseal with
the first half of the members"). A bare `reseal` would fire the template runbook on every batch
ejection payload: a 36% false-positive rate on the word alone.

## After: the regex that requires template context

```
apt automation enabled
|resea(l|ling)(\s+[A-Za-z0-9-]+){0,3}\s+template
|template(\s+[A-Za-z0-9-]+){0,2}\s+resea(l|ling)
|template[\s-]reseal
|apt-daily(-upgrade)?\.timer.*enabled
|unattended-upgrades=enabled
```

Run in both engines against a positive set and the queue-vocabulary negative set. Columns are
Python `re` and `grep -Ei`:

```
HIT  HIT  at some point you wanted me to reseal the VM template. do you have an
HIT  HIT  reseal the template
HIT  HIT  how do I reseal template 9110
HIT  HIT  template reseal
HIT  HIT  resealing the runner template
HIT  HIT  do you have an SOP for resealing the VM template
HIT  HIT  ::error::provision.sh: template 9110 has apt automation enabled; resea
HIT  HIT  the ephemeral CI runner template needs resealing
HIT  HIT  template-reseal
--- negatives ---
clear clear hand (head resealed to 34ff596).
clear clear halved: reseal with the first half of the members, let CI ju
clear clear marks the bead RED, but it does not reseal the PR
clear clear _batch_reseal() already does the mechanical
clear clear survivors in the same PR number (head= resealed).
clear clear 1. breaker: head resealed
```

9 of 9 positives, 0 of 6 false positives, identical in both engines.

## Corpus control: the whole bead store, old regex versus new

3683 beads, title plus description, both regexes:

```
old 5  new 7  corpus 3683
added: ['sp-o954i', 'sp-qk3yq']
lost: []
```

Nothing lost. Both additions are genuinely this subject — `sp-qk3yq` is the v1-advance probe
bead, and `sp-o954i` quotes `(reseal template` out of the backlog it is summarising. Measured
false-positive cost of the widening across 3683 beads: zero.

## After the write, through the real user path

```
$ printf 'at some point you wanted me to reseal the VM template. do you have an SOP i can follow for that?\n' | ./spira/sop.sh match -
sop-template-apt-drift	MATCH	1	Provision refuses with "::error::provision.sh: template <VMID> has apt automation enabled; …

$ printf 'verdict: PR 41 red — ejected sp-x, head resealed to 34ff596; survivors re-pushed to the same batch PR\n' | ./spira/sop.sh match -
                                                        (empty — correct)
```
