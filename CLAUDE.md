# Working on Spira

This file is for an agent working **on** the harness — changing its code. It is not the brief
an aeon gets when the harness summons it to work a bead; that comes from a persona in
`spira/chamber/`.

Read `README.md` first for what the thing is. This is what is different about editing it.

## The one rule that decides most questions

**This repository is cloned by people whose infrastructure is not yours.** Everything below
follows from that.

Ship the **mechanism**: the rule, the trap, the reason a guard fails closed. Those are why the
code is correct, and a colleague who deletes a guard because it looked arbitrary is a cost you
paid by leaving it unexplained.

Do not ship the **inventory**: a repository name, a deploy path, a host, a person, a bead id,
the date an incident happened. Those teach a colleague's agent to reason about a machine that
does not exist, and sometimes to act on it. Keep the scar as one generic clause — *"a remote
need not be called `origin`"* — and leave the case history wherever your own notes live.

`spira/inventory.sh` is the fence, it runs from the landing gate, and `spira/inventory-deny`
is where you add your own names. It scans comments too, because that is where all of it was.

## Configuration, not constants

Every path, name and label belongs in `spira/conf.sh` with a default derived from where the
harness is installed. A literal in five files is how five programs come to disagree; the
escalation label was exactly that, and the panel is the half nobody notices is wrong, because
it simply shows fewer.

Two keys are deliberately **not** settable from the config file: `SPIRA_HOME` and
`SPIRA_REPO`. Where the harness *is* is a fact about where `conf.sh` sits. The landing gate
extracts a branch to a scratch tree and runs that tree's suites; a config that could point
them back at the installed copy would make the gate test the code already in force, and pass.

## Tests

`spira/test-*.sh`, discovered by glob and never from a list — add one and something runs it.
Run the suites that cover what you changed while you work, and the whole gate once before you
push.

**Two places a suite can run, and adding one puts it in the second.** `spira/gate-suites`
names the few the landing gate runs on every branch, each with the reason it earns the wait;
that is the only hand-written list of suites here and adding to it is a deliberate act. The
glob's complement — everything `spira/test-*.sh` finds that the list does not name — is run by
`spira/suites.sh` on a schedule, inside the Ops session, which files a bead per red and blocks
nothing. So a new suite is executed by existing, a deleted one stops being run with no edit,
and the two sets cannot be edited into overlapping. Before this the gate's list was
unreadable from outside the gate: five of nine suites ran nowhere at all, three of them landed
the same night with their beads closed citing them as verification.

**Every suite declares what it covers**, on a `# covers:` line just above its `set -uo
pipefail`, as space-separated path globs. Err wide: a suite run needlessly costs seconds. A
`# covers:` glob outranks a file's extension, so a suite may claim prose and should where
prose is executable in effect — the personas are `.md` files. Nothing selects on those
declarations yet; they are what a later selector will read, and a selector is only sound once
a full run exists behind it. A suite that declares nothing is reported by `suites.sh` as an
omission and is still run — skipping it would rebuild the defect that program ends.
`suites.sh list` prints every suite, where it runs, and what it claims.

Three properties the existing suites have and a new one should too:

- **A check that finds nothing must first prove it could have found something.** Plant an
  offender and require the matcher to say so, then believe it when it is silent.
- **Test against the real dependency**, on a throwaway instance (`spira/testdb.sh`), not a
  hand-written model of it. A stub reproduces the surface you remember, so its gaps surface
  as failures in correct code.
- **Run in an explicit, minimal environment.** Ambient configuration silently decides
  verdicts: a suite that inherits a real `spira.conf` is asserting against one box, and one
  that inherits `SPIRA_WIKI` will write into a real wiki page.

Pin a configured value to a **non-default** in a fixture where you can. Asserting against the
shipped default passes just as well if the code has the literal written in, which is the thing
the key exists to stop.

## Prose

**Prefer no comment.** Almost everything the comments here say belongs in a name, a guard or
a test — put it there, where something runs it. A comment is the weakest place to keep a
fact: nothing executes it, nothing fails when it goes stale, and it is believed anyway.

Write one only for what the code genuinely cannot state — why a guard fails closed, why the
obvious simplification is wrong — and keep it to a line or two.

**Long comments have cost real money here.** They restate the mechanism in prose, so a matcher
looking for the mechanism finds the explanation and passes against code it was deleted from
(`law-a-matcher-reads-code-not-prose`); and an agent reading a file weighs a confident
paragraph against the code and sometimes believes the paragraph.

State the rule, not the incident. The scars live in the statute book, which is queryable and
versioned and cannot silently rot next to code that moved on; cite a slug if it helps, but do
not retell the case. Never leave a correction on top of a wrong statement — say the thing as
it now stands.

The comments already in the tree are far longer than this. Shorten what you touch; do not
open a sweep.
