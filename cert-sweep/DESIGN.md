# cert-sweep — design

Continuous certification of the landing ref's tip. The landing gate keeps its latency budget
and runs a selection; this unit supplies the coverage the gate cannot afford, and the record
that makes a red cheap to attribute.

## Passes

- `spira-cert-sweep-full` (hourly): every `spira/test-*.sh` on the round VM (`round-vm run`).
- `spira-cert-sweep-sample` (every 30 min): a random 1/`--subset-div` of the suites on this host
  (`testenv`), resampled each pass. The unit passes `--deadline` to `testenv`, which cuts the trial
  below `TimeoutStartSec`: suites it could not start are deferred, and a run that could not get
  capacity ends as `SWEEP FAULT` carrying testenv's `VERDICT` line instead of being killed.

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
was not filed — bound reached, bead.sh failed, no verdict from a rerun, the open-bead lookup
failed — is left out of the history, so the next pass sees it again; nothing is reported and
then lost.

## Attribution

A `NEW RED` or `RED` is attributed before it is filed, on the pass's own runner, after the pass:

1. Rerun the suite `--reruns` times (default 3) on the tip. Any green among them makes it a
   `FLIP` (green and red on one commit), filed as a flip bead.
2. A red that reproduces is run at the last-green commit, then bisected through the first-parent
   commits between it and the tip. The bead names the round the culprit first appears in, the
   member bead from its `merge <bead>` subject, and the first `FAIL` line of the red output. A
   red at the last-green commit too, or a bisect that cannot finish, files with the window only.
3. Before any filing, `$SPIRA_BD -C $SPIRA_DB list` is read for an open bead naming the suite
   with a flip title (flip) or a regress/basefail/red title (red); one found is reported, not
   duplicated. A lookup that cannot run holds the suite for the next pass.

Rerun verdicts are recorded as mode `rerun` rows.

## Seeding

`cert-sweep seed --results-dir D --commit SHA --round R` records a known-green run as mode
`seed`, raising no events. Without it the first red has no last-green to name.
