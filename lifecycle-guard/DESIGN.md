# lifecycle-guard — design notes

lifecycle-guard is the static analyser behind design bead-lifecycle-state-machine §3.6: it
refuses any path that reaches a bead's state around the lifecycle machine. `--gate` refuses
the classes in `GATE_CLASSES` (src/rules.rs), with no allow-list, and counts the rest on one
summary line. This file records the rules whose reasoning does not fit in a doc comment.

## bd-status-read (Rust)

Design §3.4: bd holds content, spira-lc holds state. A Rust decision that reads a work bead's
bd `status` or `assignee` is a finding (`src/bd_status.rs`), in four shapes: a bd status
value compared against a status-named operand, a `match` arm on one, a bd query filtered by
`--status`, and an `assignee` read in a condition. A work bead's state is read through
`spira_config::lc_state` (`spira-lc list` / `show`).

Scope, which is not an allow-list:

- test code (`tests/`, `tests.rs`, `*_tests.rs`, a `#[cfg(test)]` module) decides nothing in
  production;
- the `lifecycle` crate is the machine, and spira-lc's `bd_facts.rs` is the one-time
  migration classifier's reader of bd (§4);
- a bead that is not a work bead (an ask, an alert, an insight, an intake mirror, an epic, a
  hold bead, the release canary's synthetic bead) has no lifecycle row and its bd status is
  its whole lifecycle. Those reads go through `spira_config::nonwork`, naming the kind, so the
  call site says which non-work bead it means. Routing a work bead through it is a review
  finding, not a loophole. Incident beads are work beads (Ops claims them); the incident
  crate's dedup still reads bd status until sp-jgjvh moves it.

### The rowless controls

Two functions are the rule's named exceptions, listed by file and function in
`ROWLESS_CONTROLS` and argued again at each definition:

- `sentinel/src/lifecycle.rs` `rowless`
- `watchtower/src/probes.rs` `rowless_beads`

Each is the positive control for "no bead is rowless" (law-absence-needs-a-positive-control).
A bead with no lifecycle row has no state except bd's, so the only way to find one is to ask
bd which beads it considers live and look each one up in the machine. Reading bd status there
audits the machine's coverage of bd. It decides nothing about a bead the machine holds, and
it is the one place where the answer cannot come from the machine. A third entry is a design
change argued here, not a configuration. A function with the same name in another file, or a
longer name that starts the same way, is still held to the rule (unit test
`the_rowless_controls_are_named_exceptions_and_nothing_else_is`).

### The off claim record

Two more functions are named exceptions, listed by file and function in `OFF_CLAIM_RECORD`
and argued in spira-claim/DESIGN.md §8.7:

- `spira-claim/src/bd_claim.rs` `off_holder`
- `spira-claim/src/bd_claim.rs` `off_release_args`

With `lifecycle_enforce` off an aeon claims through bd (sp-860zj kept that path), so bd's
`in_progress` and assignee are the claim and no lifecycle row records it. spira-claim's
live-work guard (unpoison, deadlocked) must read that claim or it is blind, and a reopen must
release it or a refused handoff never re-enters `bd ready`. Each function is called only with
the switch off; with it on, the holder is the lifecycle row's and a reopen writes no bd status.
They go when the off path does. A further entry is a design change argued in the owning
crate's design and here, not a configuration (unit test
`the_off_claim_record_is_named_by_file_and_function`).

### Gate status

`bd-status-read` joins `GATE_CLASSES` in the commit that clears its last findings: spira-claim's
select/ready and holder paths (sp-860zj) and the incident dedup (sp-jgjvh). Until then the
gate counts it on its summary line.
