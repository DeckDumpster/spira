# 4 — CHECK 4's workload, four ways, on one fixture

`poc/lever-bench.sh <fixture>` runs the work CHECK 4 actually does over the fixture's 60
dispatchable beads, four ways, back to back on the same store in the same minute:

- **A — CLI, per bead.** The code as it stands: `bd label list <id>`, the attempts aggregate
  and the reopens aggregate, once per bead. 180 processes.
- **B — CLI, batched.** One `bd list --status open --json --limit 0` (labels come with it)
  and one `bd sql` with `GROUP BY issue_id` returning attempts and reopens for every bead.
  2 processes.
- **C — in-process, per bead.** The same 60 label lookups through the Go SDK, store opened
  once. 1 process, 60 store calls.
- **D — in-process, batched.** One list through the Go SDK. 1 process, 1 store call.

<!-- markdownlint-disable -->

    fixture=<sptest server fixture> beads=60

    variant                                    wall
    A  CLI, per bead  (3 x N calls)      31619.1 ms
    B  CLI, batched   (2 calls)            370.9 ms
    C  in-process, per bead (N)           1632.4 ms
    D  in-process, batched  (1)            152.2 ms

    positive control — the batched GROUP BY returns non-zero rows for a bead with events:
    issue_id    | count(*)
    ------------+---------
    sp-load0015 | 4
    sp-load0008 | 4
    sp-load0042 | 4
    (3 rows)

<!-- markdownlint-enable -->

Wall, at this session's 70% quota. A is 176 ms per bd call, which is the CLI's server-mode
per-call figure from `02-per-call-phases.md` and not a new number.

The four readings:

| comparison | factor | what it sizes |
|---|---|---|
| A -> B | **85x**, 31.2 s saved | batching alone, no new machinery |
| A -> C | 19x, 30.0 s saved | an in-process client with the N+1 left in |
| B -> D | 2.4x, 219 ms saved | an in-process client *after* batching |
| C vs B | C is **4.4x slower than B** | a Rust/Go harness that does not fix the loop |

The last row is the one the bead was cut to settle. **Rewriting the harness around an
in-process store client, while leaving CHECK 4's N+1 in place, lands 4.4x slower than a
forty-line change to one bash loop.** The levers are not additive and they are not
independent: batching removes the calls that an in-process client would make fast.

C's 1,632 ms includes the probe's own ~50 ms process start and ~50 ms store open, amortised
over 60 calls; the per-call figure is about 26 ms, against A's 176 ms.

C and D do the label half of CHECK 4's work only. The bd Go SDK's public `Storage` surface
exposes no raw-SQL method, so the two `events` aggregates have no in-process equivalent
through it — they would have to be re-expressed against whatever event query the storage
layer exposes, or the SDK extended. That is unmeasured and is one of the costs option D
carries.
