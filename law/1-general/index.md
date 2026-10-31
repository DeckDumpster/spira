# Part 1 — General

Placeholder Part until the table of contents is approved.

- §1.1 law-absence-needs-a-positive-control — A check that finds nothing must first prove it could have found something.
- §1.2 law-a-watcher-runs-the-sop-before-it-pages — A watcher that detects a condition runs the tool its matching SOP names, records the application, and reports the outcome; it escalates to the Concierge only what no runnable SOP covers, or what the tool could not settle.
- §1.3 law-a-failure-that-will-not-reproduce-is-a-fidelity-gap — A failure that repeats in a round or CI but passes in a reproduction means the reproduction is wrong.
- §1.4 law-p0-express-preempts-lanes — Maintenance lanes never hold capacity a P0 needs: while a P0 or express bead waits unclaimed, no lane dwarf is called, and a waiting P0 express preempts the youngest running lane dwarf through the stop path, which makes its bead true and loses nothing.
- §1.5 law-a-test-that-flips-is-deleted — A test that fails then passes on an unchanged tree is deleted in the same round, not retried or quarantined, and a follow-on bead filed with the deletion adds a deterministic test restoring coverage parity for what it covered; it need not be the same test.
- §1.6 law-an-event-loop-never-blocks — An event loop never blocks: every blocking call it would make, file, socket, process, lock or sleep, is dispatched through a bounded queue to a bounded thread pool, and the loop only awaits the result.
- §1.7 law-a-reconciler-redrives-it-does-not-alarm — A reconciler redrives; it does not alarm.
- §1.8 law-a-p0-in-spira-itself-is-built-by-hand — A P0 defect in Spira's own machinery is built by the Concierge by hand and joins the very next round, cut at once with whatever is certified: it never waits for a batch to fill, and it never lands alone while certified work waits behind it.
- §1.9 law-operator-asked-transcript-reads-are-observation — The Concierge may read any live dwarf's session transcript read-only, asked or not: it is observation, like a status check.
- §1.10 law-doomed-work-is-stopped-fixed-then-resumed — Work the Concierge can see is doomed — a builder rebasing a member its open integration will redo, a round member whose tree cannot pass setup, a bead stacked on a parent that will not land first — is stopped at once: ejected from its round, or slain and held with a manual hold.
- §1.11 law-unit-tests-run-under-a-second — A unit test runs in 10 to 100 ms against a pure core: no database, git repo, container, sleep or other program.
- §1.12 law-diagnose-before-you-buy-capacity — When a run suddenly takes longer, find the cause before changing a capacity knob: compare per-suite and per-phase timings of a fast pass and a slow one, file what you find, then choose between fixing the cause and buying capacity.
- §1.13 law-a-kit-mechanizes-the-sites-it-never-competes — Shared-platform code mechanizes the pattern its sites already run correctly; it never builds a parallel path beside it.
- §1.14 law-the-harness-switches-his-cockpit — Whatever puts a design, diff or report in front of Ryan switches his cockpit to it itself, every iteration; it never asks him to switch, and only he dismisses it.
