# Architecture

## Components and stores

```
                  +------------------- systemd (user + system) -------------------+
                  |  timers: sentinel, summon, sift, rounds(-forge), gate-worker,   |
                  |  landing-pass, verdict, czar-pass, watchtower, skew, hel... |
                  +---+-----------------------------------------------------------+
                      |
 operator / Concierge |                                       aeon unit (transient)
 (mail, panes) -files-+--> bead.sh / work file               spira-aeon-<rune>-<n>
      ^               |        |                             +------------------+
      | escalations   v        v                             | aeon (runner)    |
      |         +-------------------------+  claim / renew   |  claude -p ...   |
      +-- mail -|  spira-lc (lifecycle    |<-----------------|  model session   |
                |  service, Unix socket)  |  work <verb>     |  `work` client   |
                |  bead/delivery/batch/   |----------------->|  (no bd, no db)  |
                |  ask machines, CAS      |                  +--------+---------+
                +-----------+-------------+                           | commits
                            |                                         v
                            v                                git worktree + branch
          +-------------------------------------+            spira/<bead-id>
          | Dolt SQL server (dolt-beads.service)|                     |
          |  - beads DB     (content, via bd)   |                     v
          |  - spira_lifecycle (state, events)  |        +---------------------------+
          +-------------------------------------+        | managed git repositories  |
                            ^                            |  landing ref, spira/<id>  |
 sentinel, spira-claim, sift|                            |  branches, round branches |
 batcher, landing-pass,     +-- read/write via --------- +-------------+-------------+
 queue, hel, strand ...    spira-lc / bd                           |
                                                                       v
 run/ ($SPIRA_RUN)                                          <releases>/<sha>/
   verdicts/trees/<repo>/<tree>  tree certificates          <releases>/current -> <sha>
   queue/<repo>/...              open-round records
   gate-worker/                  gate queue + locks
   tsd/ otel/ watchd/ ...        time series, spans, watcher logs
```

| Store | Where | Holds | Written by |
|---|---|---|---|
| Beads database | Dolt server (`dolt-beads.service`), `$SPIRA_DB` | bead content, labels, deps, comments, statute and SOP memories | `bd` (through `bead`, `rule`, `sop`) |
| `spira_lifecycle` | same Dolt server | `bead`, `delivery`, `batch`, `batch_member`, `ask`, `mending`, `mend`, append-only `event`, `bead_dep` | **only** `spira-lc` (a distinct database user holds the grants; the event table is insert/select only) |
| `run/` state | `$SPIRA_RUN` | tree certificates, open-round records, gate queue and locks, time-series rows, spans, watcher logs, pass state | the binaries that own each file |
| Git repositories | the registry's paths | branches `spira/<id>`, round branches, the landing ref, `refs/archive/rounds/<n>` | aeons (their branches), `batcher` / `queue` (rounds, landing) |
| Releases | `$SPIRA_RELEASES` | immutable releases and the `current` link | `release` |

`spira-lc` serves the machines over a Unix socket (`SPIRA_LC_SOCKET`). On a multi-user install
it runs as `spira-lc.service` / `.socket` under its own system user, so the database
credential is unreadable by anyone else; in same-user mode it is `lc-serve.service` /
`.socket` on the user manager's runtime directory. The same `dispatch` code serves the socket
and the one-shot CLI, so they cannot drift. `spira-lc` exit codes: 0 applied, 1 no row,
2 cannot tell (store unreachable; safe to retry), 3 refused (not safe to retry).

## A bead from filing to released

This follows the `queue.local` path with default timer cadences; each step names the binary
and unit that does it.

1. **File.** A bead is created: `bead.sh file`, `work file` / `file-followup` / `split` from a
   session, `gh-intake` from GitHub issues, or a trigger unit (`spira-groom`, `spira-warden`,
   `spira-mimir`, `watchtower`, `incident`). `bead` also writes its lifecycle row in `READY`.
   A label places it in a persona's partition (the plan label, for builders).

2. **Sentinel and summon.** `spira-sentinel.timer` (every 2 min) runs `sentinel`;
   `spira-summon.timer` (every minute) runs `sentinel --summon-only`. The pass counts ready
   beads per partition and summons aeons: lane personas against their own caps, then task
   personas from the shared `SPIRA_MAX_AEONS` pool, builders last as the elastic persona. The
   same pass reaps `WORKING` rows whose lease expired (`HolderDead`), applies the poison hold
   when attempts are exhausted, holds blocked beads on `wait`, and dispatches the landing
   worker (step 7).

3. **Claim.** The summoned `aeon` asks `spira-claim select` for the top-ranked claimable bead
   (`READY`/`REWORK`, no hold but `wait`, in its partition, blockers satisfied; epics first,
   express beads ahead) and sends `Claim` to `spira-lc`. The compare-and-swap means two aeons
   cannot win the same bead. The claim records holder, persona, lease and, for a bead built on
   unlanded prerequisites, the stack of their certified tips.

4. **Aeon session.** `aeon` creates the worktree and branch `spira/<id>`, renders the persona
   brief (`<name>.md` plus statutes), and launches the model with the restricted environment.
   It reports `Phase`, renews the lease while the trace grows, and ends the session with the
   work preserved if the lease lapses. The model commits to its branch and runs tests only
   through `testenv`.

5. **Submit.** The model runs `work submit`. The client reads the tip from
   `refs/heads/spira/<id>` (never from an argument); `spira-lc` refuses if the tip is one a
   round ejected as red, if the stack is stale, or if a lint rule or the declared proof tier
   fails; otherwise `Submit` moves the row to `SUBMITTED`. Alternatives: `work done` closes a
   bead with nothing to land; `work blocked` holds it and files an ask. At exit the aeon's
   teardown decides whether the attempt is charged, and the unit's stop hook runs
   `sentinel --summon-only` to refill the slot.

6. **Screen.** `spira-sift.timer` (every minute) runs `batcher sifts`, which screens each queue
   repository's proto-round (all `SUBMITTED` and `CERTIFIED` rows). A pass is recorded as
   `Sifted {tip}`; a conflict, duplicate or policy gap sends the bead back, supersedes it, or
   files a mend. The batcher cuts only beads whose current tip has a screen pass.

7. **Per-branch gate (optional certification).** `landing-pass land` (dispatched by the
   sentinel) queues each done branch for `gate-worker` (`spira-gate-worker.timer`, every 30 s),
   which runs up to `certify_par` gates at once under host admission control and files each
   verdict. `landing-pass` applies it: `PASS` → `GatePass` (`CERTIFIED`), `FAIL` → `GateRed`
   (`REWORK`, charged), `NO_VERDICT` → `GateInfra`, `BASE_FAIL` → one incident per repository.
   A round does not require a gate verdict: its full corpus is the stronger trial.

8. **Cut.** `spira-rounds.timer` (every 30 s) runs `batcher rounds-local`, cutting one
   `queue.local` repository at a time; `spira-rounds-forge.timer` runs `batcher rounds-forge`
   for `queue.forge` repositories. A cut picks the round (feature-first: the epic with the most
   pool members, plus what they stack on and every express bead; else the whole pool), merges
   the members, and creates the batch row and, per member, a delivery row. Members move to
   `IN_DELIVERY` (`Deliver`). While one round's suites run, the next may be staged behind it.

9. **Gate on the round.** The round's full corpus runs through `testenv` on a round VM
   (`round-vm`, a pooled warm VM; `spira-round-vm-drain.timer` releases an idle one). Each pass
   is recorded on the batch row. A red is attributed first by path (reds intersected with each
   member's diff), then by a mender, then by the batcher judge. Members at fault are ejected
   (`Returned` → `REWORK`, with quoted evidence) and the rest re-run. A green writes a round
   certificate for the exact tree under `verdicts/trees/<repo>/<tree>`.

10. **Settle.** `spira-verdict.timer` (every 2 min) runs `queue step --all`, which settles open
    batches of queue repositories; `pr`-mode repositories are walked by
    `spira-landing-pass.timer` (every 90 s).

11. **Land.** `queue land-local` refuses unless a round `GREEN` certifies the head's tree. For
    the harness repository it first builds and verifies the release from the round's own
    tested binaries, so a release that cannot verify never lands. Then it fast-forwards the
    landing ref with a compare-and-swap on the base sha, archives the head under
    `refs/archive/rounds/<n>`, closes the beads, and walks each member on `spira-lc`:
    `Deliver`, then `Delivered` → `LANDED`. A later fault never undoes the landing; a failed
    `Delivered` is reported and the ref stays. Waiting beads are rebased onto the new base.

12. **Cleanup.** `hel` deletes the branch and worktree of every bead whose work has landed,
    and nothing else; the sentinel runs it each pass, `spira-straggler-sweep` covers queue
    repositories hourly, and `spira-worktree-reaper` removes stale worktrees. Content found
    already on the base is closed by `ContentOnBase`.

13. **Release.** After the ref moves, `release activate <sha>` switches `current` onto the
    verified release and restarts running units, waiting a bounded time for passes in flight.
    A deploy fault leaves `current` where it was, writes a fault marker, and makes `land-local`
    exit non-zero (the landing itself stays). Under `queue.local`, a later publish step carries
    the commit to the forge through delivery states `PUBLISHING` → `PUBLISHED` or
    `PUBLISH_RED`; a red publish is fixed forward on a new bead. `skew` checks hourly that the
    harness in force is the harness that landed; `release rollback` re-activates the previous
    release.

## Watching the loop

Several units exist only to notice the loop failing and turn that into work or mail rather than
silence: `watchtower` (hands `ops` the pipeline's vital signs), `czar-pass` (30 s queue-stall
detection with deterministic remedies), `reconciler` (declared structural invariants: remedy or
escalate), `heimdall` (escalates a stalled loop and does nothing else), `strand` (stranded-work
detector), `cert-sweep` (re-certifies the landing ref's tip on a schedule), `refusal-watch`,
`mend-watch`, and the `spira-lc event-continuity` check. `cockpit-collect` and `cockpit/`
present the same stores to the operator.
