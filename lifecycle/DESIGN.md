# lifecycle — design notes

lifecycle is the pure transition machines for a bead's state (bead, delivery, batch) and the
event-log fold that replays them. Every function is pure; evidence arrives on the event.

## Scope: beads

The machine governs beads. A thing that is not a bead is outside it, and nothing here models
it:

- an ask is not a bead: it has its own machine (`ask`: OPEN, then ANSWERED, DEFAULT_TAKEN or
  WITHDRAWN, each exit carrying who and the quoted words), so no list or claim over the bead
  machine can return one. Its decision bead in the store carries the text only.
- a non-work bead (alert, insight, intake mirror, epic, hold bead, incident bead) has no
  lifecycle row; its state is bd's `status`, read through `spira_config::nonwork` naming the
  kind. An incident bead is non-work: a work bead that resolves one may refer to it and takes
  no state from it.
- a suite-state branch is not a bead. It stays on the queue's own record, a specific written
  exception argued in `queue/DESIGN.md` "Suite-state branches"; it never gets a lifecycle row.

## Reaching the state

Code outside this crate reaches a bead's state through spira-lc. lifecycle-guard refuses the
rest. A guard finding is cleared exactly three ways — move the access to spira-lc, delete it,
or argue a named exception in the owning crate's design — and never by an allow-list. The
rule and its one exception, the rowless controls, are in `lifecycle-guard/DESIGN.md`.
