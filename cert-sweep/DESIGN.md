# cert-sweep — design

Continuous certification of the landing ref's tip. The landing gate keeps its latency budget
and runs a selection; this unit supplies the coverage the gate cannot afford, and the record
that makes a red cheap to attribute.

## Passes

- `spira-cert-sweep-full` (hourly): every `spira/test-*.sh` on the round VM (`round-vm run`).
- `spira-cert-sweep-sample` (every 30 min): a random 1/`--subset-div` of the suites on this host
  (`testenv`), resampled each pass.

Both read the tip of `--base` (default `local/main`) and label it with the
`refs/archive/rounds/<n>` that points at it (`?` if none does).

## History and events

Every result is a row in the `cert-history` tsd family: `commit round suite verdict secs mode`.
`verdict` is `ok | red | fault | skip`. A suite that podman lost track of is `fault`, its own
state: it is never folded into a red and never moves a culprit window.

An event is raised once per transition, into the `cert-event` family and stdout:

| event | when |
|---|---|
| `NEW RED` | first red after a green; names the last-green and first-red rounds — the culprit window |
| `RED` | red with no green anywhere in history; no window to name |
| `FLIP` | green and red on one commit; the suite is reported for deletion |
| `FAULT` | a verdict podman lost |
| `SWEEP FAULT` | the runner returned no results |

`NEW RED`, `RED` and `FLIP` file a fix bead for the builder at `--priority` (default 1,
raised: detection outranks rejection), at most `--max-beads` per pass. A transition whose bead
was not filed — bound reached, or bead.sh failed — is left out of the history, so the next pass
sees it again; nothing is reported and then lost.

## Seeding

`cert-sweep seed --results-dir D --commit SHA --round R` records a known-green run as mode
`seed`, raising no events. Without it the first red has no last-green to name.
