# Time-only waits and gates

Rule: a wait names the object or signal it waits on and carries a cap
(law-a-wait-names-what-it-waits-for). `spira-lint --only time-only-wait` refuses a new
sleep-then-proceed, a loop of sleeps with no exit condition, an uncapped poll, and a stamp or
gate that lifts by the clock. A wait that is genuinely on a named object but looks like one of
those marks its line `# waits-on: <object>; cap <bound>`.

`spira-lint/time-only-wait-allow` is the shrink-only list of offenders below, each with the bead
that converts it.

| Where | Really waits for | Observable? | Converting bead |
|---|---|---|---|
| `concierge.sh` 499, 524 — `sleep 3` after new-session | the tmux session and pane to exist | yes: `has-session`, `list-panes` | sp-4gcxuz.2 |
| `concierge.sh` 613, 614 — wake settle sleep, uncapped input-busy poll | the pane's input line to be idle | yes: `capture-pane` | sp-4gcxuz.2 |
| `release/src/acceptance/phases.rs` — `sleep(30)`, `sleep(120)` | spira-* units to be active or failed | yes: `systemctl list-units` | sp-4gcxuz.3 |
| `release/src/activate.rs`, `release/src/install.rs` — `settle` | restarted units to be active or failed | yes: `systemctl` | sp-4gcxuz.4 |
| `round-vm/src/pool.rs` — fixed `retry_interval` between acquires | a slot to free | yes: `list_vms` | sp-4gcxuz.5 |
| `spira/inbox-append.sh` — `PUSH_WINDOW` | no further append to the spool | yes: spool size / mtime | sp-4gcxuz.6 |
| `store-persist/probe/probe.sh` — `while kill -0` | the pid to exit | yes, but uncapped | sp-4gcxuz.7 |

## Known, not detected

The lint reads sleeps and expiry stamps. Two entries the bead named are waits whose loop shape
it cannot judge, and were not found as a bounded "wait for live aeons" loop in this tree at the
time of writing: the rounds-loop / round2 live-aeon waits, and the cert / landing "settle"
waits. They are not on the allow list because the rule does not flag them; a rule that learns to
read them adds them here with their bead.

## What the rule does not see

A capped loop that polls an unrelated condition passes (the cap check is a keyword match), and a
fixed backoff in a loop is not flagged. The rule refuses the shape of a time-only wait; it
cannot tell that a named condition is the right one.
