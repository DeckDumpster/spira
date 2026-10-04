# groomer — graph hygiene operations for the Spira DAG

Rust-rewrite wave 7c (sp-aufxu). Replaces `spira/groomer.sh` and
`spira/groomer-litter-predicate.py`.

## Intent

The groomer persona (`chamber/groomer.md`) does graph hygiene — splitting beads that
cannot land on their own, merging duplicates, closing premise-gone litter, correcting
mislabelled lanes, and triaging `spira-poison` — and this crate is its tool, not its
judgement. Every subcommand is a **mechanical** operation: it either does exactly what it
is told (with required evidence, so a lift or a close is never indistinguishable from an
ungrounded amnesty) or it refuses outright. The one judgement call this crate makes itself
is `sweep`'s mechanical remedies, and those are computable, not discretionary — the STATE
and LIVELOCK detectors that back them already draw the line between "fully computable"
and "needs a human or a model reading the bead" (see `lib.sh`'s own comments on
`detect_livelocked` and its STATE-scan siblings).

## Contract

```
groomer sweep          [--dry-run]
groomer split-piece    <original-id> [bd create args...]
groomer supersede      <id> --with <successor>
groomer close          <id> --evidence <text>
groomer correct-lane   <id> --lane <lane>
groomer depends-on-fix <bug-id> --fix <id> --evidence <text>
groomer unpoison       <id> --cause <c> --evidence <text>
groomer triage-poison  <id> --verdict <work-fault|drop> --evidence <text>
groomer deadlocked     [--apply]
groomer unwanted       ...                                   REFUSED — exits 2 always
```

Exit: 0 success, 1 usage error / missing required argument, 2 refused (a groomer policy
refusal, currently only `unwanted`).

**`unwanted` is refused in the code, not in a sentence in a brief.** Closing a bead as
unwanted changes the backlog's declared desired state — a POLICY call that belongs to
Ryan by the escalation policy — so there is no argument shape that makes this subcommand
succeed.

**Every close and every poison action takes required evidence.** `close --evidence`,
`unpoison --cause --evidence`, `triage-poison --verdict --evidence`: a lift or a close
with nothing behind it is indistinguishable from an ungrounded amnesty, so the CLI itself
refuses to run without it.

## What stays in lib.sh, and how this crate reaches it

The detectors `sweep` drives — `detect_livelocked`, `detect_incident_needs_builder`
— and the write-side helpers `bead_reopen`, `bump_poison_cleared`, `poison_asked_clear`
all live in `spira/lib.sh` and stay there: lib.sh is the rewrite programme's own group 4,
proposed last, and Ryan's standing instruction during the cutover was "leave lib.sh
alone." They are also shared with callers this crate does not own —
`cockpit.sh livelock` calls `detect_livelocked` directly, `attempts.sh` calls
`bump_poison_cleared`/`poison_asked_clear`, `incident.sh` and `auron.sh` call
`bead_reopen` — so re-deriving their logic here would be a second copy of behaviour
several other scripts depend on staying exactly as it is.

`groomer` reaches them the way `sentinel` reaches its own lib.sh seams and the way
`rebase-stale::seam::Seam` reaches `lib.sh`'s bead-store helpers: `bash -c '. "$LIB";
"$@"' lib.sh <func> <args…>` (`src/seam.rs`, the `Seam` trait). Production shells out for
real; every test in this crate runs against a recording `FakeSeam` instead, so the
sweep's dispatch logic is unit-tested without a live bead store or a real lib.sh load.

`unpoison` similarly delegates entirely to `spira-claim unpoison --credit <cause>`
(`spira-claim/DESIGN.md` §8) — the unjudged credit, the `poison.cleared` floor, the
lifecycle hold, the note and the ask are spira-claim's; this crate only builds the
invocation and judges the `OK   <id>:` prefix `groomer.sh` always checked.

`groomer sweep`'s `ci-stuck` and `incident-is-code` remedies call `spira-lc unhold` — an
already-Rust binary, invoked by bare name on the release PATH, same as `spira-claim`.

## Schema

`sweep`'s detectors speak two textual line formats, parsed by `src/sweep.rs::parse_*`
(pure, table-tested):

- `LIVELOCK <id> <category> — <reason>` — categories `ask-no-overseer`, `ci-stuck`,
  `unmapped-repo`, `unclaimable`.
- `STATE <id> <kind> [<extra>] — <evidence>` — kind `incident-is-code`.

Everything else in this crate is a direct 1:1 argument mapping onto `bd` verbs
(`src/bd.rs::Bd`), kept behind a trait for the same reason as the lib.sh seam: every
subcommand's argument construction is unit-tested against a recording `FakeBd`, matching
the bash suites' `STUB_BD` argv-recording technique without a subprocess.

## Decisions (what was dropped, and why)

- **`deadlocked` was added mid-rewrite, by a different bead, to the bash this crate was
  already replacing** (sp-rfodk, landed after this wave started: `attempts.sh
  deadlocked` moved into `spira-claim deadlocked`, with `groomer.sh deadlocked` as its
  git half — spira-claim/DESIGN.md §9). Ported here (`src/deadlocked.rs`) rather than
  left behind on the deleted script: it gathers every poisoned bead across the roster
  (`Seam::all_partition_members`), judges each one's git state exactly as the bash did
  (repo map lookup, branch existence, land base, a commit naming the bead, a clean
  merge-tree), and hands the verdicts to `spira-claim deadlocked` as the same bare JSON
  array on `--merge-status`, unchanged.
  - **The non-enforce poison check is `contains`, not the landed bash's `grep -qx`
    (exact whole line).** Checked against a live `bd label list <id>`: the real output is
    never a bare label per line — `🏷️ Labels for <id>:` then `  - <label>` rows — so an
    exact-line match can never fire. `groomer.sh`'s own `triage-poison` case already used
    the substring form for this reason; `deadlocked`'s `grep -qx` looks like a bug in the
    bash this crate is replacing, not a behaviour to reproduce, and lifecycle_enforce's
    path (`spira-lc held <id> poison`) is unaffected either way.
- **`groomer-litter-predicate.py` is gone, not repointed.** It was pure JSON-in,
  judgement-out with no dependency the Rust binary doesn't already have (`serde_json`),
  so keeping it as a subprocess `groomer` shells out to would be paying a process spawn
  for logic that fits in ten lines (`src/litter.rs::judge`). Ported with its exact
  fail-open behaviour (malformed JSON, missing fields, blank description) and its
  sanitising regexes reproduced as character filters.
- **The `$HERE/conf.sh` sourcing groomer.sh did for `SPIRA_CI_LABEL`,
  `SPIRA_INCIDENT_LABEL` and `SPIRA_PLAN_LABEL` is now three on-demand reads through the
  same lib.sh seam** (`Seam::conf`), rather than a second config-resolution path
  (`spira-config`) duplicating what sourcing lib.sh already gives for free — lib.sh itself
  sources conf.sh, so one seam call sees both.
- **groomer.sh's own bash-side argv validation (`--with requires a value`, `unknown
  option: %s`) is reproduced verbatim in `src/main.rs::parse_flags`**, including exit code
  1 for every case, so the CLI's error surface does not change shape for any caller
  (aeon.sh, the chamber brief, an operator at a terminal) mid-rewrite.
- **Every write verb discards bd's own stdout, keeping stderr visible.**
  `groomer.sh` redirected most of its write calls to `/dev/null` (`split-piece`'s
  `set-state`/`label remove` most pointedly — `split-piece`'s own stdout contract is the
  new id alone, and letting bd's confirmation text leak into it broke exactly that: a
  caller capturing `$(groomer split-piece …)` got bd's "✓ Set branch = …" text ahead of
  the id, caught by `test-groomer-split-piece.sh` against a real store). `label
  add`/`remove`, `note`, `set-state`, `supersede` and `dep add` all get the same
  treatment in `RealBd::run_inherit` — consistent, and nothing in this crate's suites
  needs one of them to print bd's own chatter. Calls the bash captured for their value
  (`show --json`, `label list`, `create`) still capture here, for the same reason.
- **Dropped: nothing behavioural.** Every subcommand, every flag, every exit code and
  every refusal in `groomer.sh` has a caller-facing equivalent here. What moved is *where*
  the logic lives (Rust, unit-tested) and *how* it reaches lib.sh (a seam, not a source).

## Test strategy

Every subcommand's argument validation, every `sweep` remedy, and the litter predicate's
full fail-open table are unit tests (`cargo test -p groomer`) against `FakeBd`/`FakeSeam`
— CPU-bound, no container, no Dolt server. What genuinely needs a live bead store and real
git ancestry (branch inheritance on `split-piece`, the STATE scan against a seeded fixture
database) stays in the repointed bash suites (`test-groomer-split-piece.sh`,
`test-groomer-state.sh`, `test-groomer-sweep.sh`, `test-groomer-incident-reroute.sh`,
`test-groomer-poison-triage.sh`) run through `testenv`, now invoking the `groomer` binary
by bare name instead of `groomer.sh`.
