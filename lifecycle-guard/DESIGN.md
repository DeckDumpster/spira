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
  hold bead, the release canary's synthetic bead, an incident bead) has no lifecycle row and its
  bd status is its whole lifecycle. Those reads go through `spira_config::nonwork`, naming the kind, so the
  call site says which non-work bead it means. Routing a work bead through it is a review
  finding, not a loophole. An incident bead is a non-work bead: its state lives in bd. A work
  bead that resolves one may refer to it, and the reference grants the work bead no state of
  the incident's.

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
change argued here, not a configuration, and `rowless_controls_match_the_design` fails when
`ROWLESS_CONTROLS` and the list above disagree. A function with the same name in another file, or a
longer name that starts the same way, is still held to the rule (unit test
`the_rowless_controls_are_named_exceptions_and_nothing_else_is`).

Each rowless control carries a comment at its definition naming the exception and pointing
here. A new call site of the same kind states its reason there the same way.

### Clearing a finding

A finding is cleared exactly three ways:

1. move the access to spira-lc (`spira_config::lc_state`);
2. delete the access;
3. argue a named exception in the owning crate's design, as the rowless controls are argued
   above, and name it in the rule's source by file and function.

Never an allow-list file, a suppression comment, or a pattern that exempts by shape. An
exception that is not named in a design and in `ROWLESS_CONTROLS` is a finding.

### Gate status

`bd-status-read` is in `GATE_CLASSES`: it joined in the commit that cleared its last
findings — spira-claim's select, epic and holder paths (sp-mve9i, after sp-860zj/sp-7g5q6
moved ready and claim) and the incident family's dedup (sp-jgjvh). A Rust decision on a work
bead's bd status is now a red at the gate (`gate_mode_refuses_a_rust_bd_status_read`).

## bd-read-rust

`ready` or `events` handed to bd as a Rust argument vector (`src/bd_read.rs`). bd's ready set
is its status and assignee, and a bead's events are the old attempt ledger; both are the
lifecycle row's. It is reported by a plain run and counted on the gate's summary line, and is
not in `GATE_CLASSES` yet: the sentinel's ready snapshot (`sentinel/src/store.rs`) and
spira-claim's `READY_ARGS_BASE` still ask bd for the set the open-children and queue-wait
label decisions read. It joins the list in the commit that moves those onto `lc_state`.
