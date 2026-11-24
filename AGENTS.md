# Working on Spira

This file is for an agent working **on** the harness — changing its code. It is not the brief
a dwarf gets when the harness calls it to work a bead; that comes from a persona in
`spira/runes/`.

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

The tree carries no shell suite: tests are Rust (`cargo test`, `tests/` under `--features
integration`, and `sim/` for behaviour over time). `no-new-shell-suite` refuses a new
`spira/test-*.sh`, and an empty suite corpus selects nothing rather than refusing.

Three properties a test here should have:

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
