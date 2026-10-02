# archivist — rescue a full session's unfinished business before it is cleared

Rust-rewrite wave 7c (sp-aufxu). Replaces `spira/archivist.sh`.

## Intent

Context is re-read in full on every turn, so a long session costs many times a fresh one
for identical work, and the fix — clearing — is exactly what nobody dares do, because a
session near the ceiling is also the session carrying the most that was never written
down: questions asked and never answered, findings stated and never filed, verdicts acted
on and never recorded. The archivist makes clearing cheap by making that loss impossible.

It reads the transcript, not the conversation, so persisting a session's unfinished
business costs the session it rescues nothing — no turn, no tokens, no interruption. The
sweep is a timer (`spira-archivist.timer`/`.service`), not a check hung off the sentinel:
the sentinel returns the moment the bead graph is healthy, which is exactly when a session
at the keyboard is most likely to be quietly filling up.

## Contract

```
archivist sweep            every live session; archive the ones that have drifted
archivist list             what the sweep can see, and what it would do about each
archivist now [<session>]  archive one session now, whatever its context (the manual
                            path — "hibernate", before a deliberate clear)
archivist mark <s> <state> [<n>]   the running archivist's own progress writes
archivist state [<s>]      print what the status line and the dashboard are reading
archivist digest <t> [<from-turn>] [--full]
                            the transcript rendered small enough for an agent to read
archivist record <line>    register one durably-filed finding for today's digest
archivist digest-send      mail today's digest (at most once a day) and empty the queue
```

Exit: `sweep`/`mark`/`record`/`digest-send` follow their own success/failure; `now`
propagates the archive run's exit code; an unknown subcommand exits 2.

**A session is swept when its turn count has drifted past `SPIRA_ARCHIVIST_EVERY` turns
since the last successful archive — never by context depth alone.** Cost is proportional
to turns since the cursor, not to how deep the context is; context tells you the session
is expensive, turns-since-cursor tells you work is uncovered. A session whose last run
failed is not re-fired by drift alone (`failed` is excluded; `timeout` is not — a
timed-out run may succeed on a later pass with a scaled timeout, up to
`SPIRA_ARCHIVIST_TIMEOUT_RETRIES`).

**No two archives run concurrently, even across entry points** (`sweep` and `now`): an
archivist-wide flock plus a per-session flock, both released the instant the holding
process exits however it exits, because the guard is a file descriptor, not a trap.

**Account capacity is a global, cooperative pause**, not a per-session concern: a
`capacity_reset_at` refusal on the session log pauses the whole harness
(`capacity_pause_set`, shared with `aeon.sh`'s own capacity checks) and returns
`ARC_RC_CAPACITY` so the sweep loop stops the rest of its pass rather than launching one
`claude` per remaining candidate for the account to refuse in turn.

## What stays in lib.sh, and how this crate reaches it

`capacity_paused`, `capacity_pause_set` and `capacity_reset_at` live in `spira/lib.sh` and
stay there — they are shared, cross-process state (`aeon.sh` reads and writes the same
pause file) and lib.sh is the rewrite programme's own group 4, proposed last, with Ryan's
standing instruction to leave it alone. `archivist` reaches them the way `sentinel` and
`groomer` reach their own lib.sh seams: `bash -c '. "$LIB"; <call>'` (`src/seam.rs`, the
`Seam` trait). Production shells out for real; every test in this crate runs against a
recording `FakeSeam`.

`system_prompt_split` and the transcript `slug` rule (`live_transcripts`'s exclusion of the
harness's own sessions) are ported **directly into Rust**, not through the seam: both are
pure string/path manipulation with no shared state any other script reads or writes.
lib.sh's own copies are untouched and still serve every other caller (`aeon.sh`,
`incident.sh`, and — for `slug` — `concierge.sh`'s comment pointing at this crate instead
of reproducing the rule a second time).

`ctx-meter.sh env <transcript>`, `archive.sh lineage <session> --json` and
`mail send operator …` stay external tools, invoked by bare name exactly as the bash
did (`mail.sh` itself was rewritten and retired by sp-ooh1k); none of the three are in
this rewrite's scope. The agent itself (`${SPIRA_AGENT:-
claude}`) is run through `timeout`, unchanged — `timeout`'s own exit code 124 is still
what says "killed by the clock."

## Schema

The per-session state file (`$SPIRA_RUN/archivist/<session>.state`, `src/state.rs`) is a
contract with two readers — `ctx-meter.sh`'s status line and `cockpit/health.sh`'s
dashboard — unchanged: `state=sweeping|archiving|safe|failed|capacity|timeout`,
`at_turn=<n>`, `items_filed=<n>`. `at_turn` is load-bearing: both readers demote a stale
verdict to "safe as of N turns ago" on this number, and it is written as the work happens,
not only at the end.

Two more of this crate's own private files, read and written by nothing else:
`<session>.covered` (`turn=<n>`, the high-water mark `set_covered` only advances on
success) and `<session>.timeout_count` (a bare integer, the retry budget).

## Decisions (what was dropped, and why)

- **The transcript scan (`live_transcripts`) and the newest-transcript pick (`now` with no
  argument) are ported directly to Rust** (`src/transcripts.rs`), replacing two inline
  `python3` scripts — both are pure filesystem walks with no dependency the Rust binary
  doesn't already have.
- **The `digest` transcript renderer is ported directly to Rust** (`src/digest.rs`),
  replacing the largest inline `python3` script in the original. Table-tested against the
  same turn-counting rule (one per distinct assistant message id), the same tool-call vs.
  tool-result asymmetry, and the same 160/300-character truncation — verified byte-for-
  byte against the Python it replaces on representative transcripts (see the delivery
  report's parity evidence).
- **`lineage_brief`'s JSON-lines parsing is ported directly to Rust**; `archive.sh` itself
  is untouched and still called exactly as before.
- **The sort-candidates-by-drift step is ported directly to Rust** (`src/candidates.rs`),
  using Rust's stable `sort_by` — matching Python's documented stable
  `sorted(..., reverse=True)` tie-break (ties keep discovery order).
- **`TZ=<tz> date +%F` (the digest's once-a-calendar-day dedup) stays an external `date`
  call** (`Seam::today`) rather than a from-scratch IANA timezone implementation — the DST
  database is exactly the kind of thing this crate should not carry a second, partial copy
  of.
- **Two flocks (`src/lock.rs`)** replace the bash's `flock 8`/`flock 9` subshell-with-two-
  fds pattern, using the same `libc::flock` idiom `queue::lock` already established in this
  workspace — a guard that is a file descriptor closed on drop, not a trap that has to
  remember to clear it.
- **Dropped: nothing behavioural.** Every subcommand, every config key, every state
  transition and every log line's meaning in `archivist.sh` has a caller-facing equivalent
  here.

## Test strategy

Every pure module (`state`, `candidates`, `digest`, `prompt`, `config`) is table-tested
directly. `transcripts` and `lock` are tested against a real temporary filesystem (real
`flock`, real file times) — deterministic and CPU/IO-bound, no container. `lock`'s
"a second taker is refused until the first drops" case holds the first lock from a
`testkit::ChildGuard`-spawned child, not an in-process fd (sp-os3of): a concurrent fork
anywhere in the `cargo test -p archivist` binary can duplicate an in-process fd and keep a
flock held past this process's own drop, which flipped the equivalent spira-config test at
round 209. `run`'s
orchestration (`sweep`, `archive`, `digest_send`, `mark`, `state`) is tested against
`FakeSeam`, covering the capacity-pause skip, the per-pass budget, the timeout retry
budget and its exhaustion, the capacity-refusal pause, and the digest's same-day dedup —
all `cargo test -p archivist`, no Dolt, no bd. The genuinely end-to-end behaviour (a real
stub `claude`, real `ctx-meter.sh`, real flocks under a timer) stays in the repointed
`test-archivist.sh`, run through `testenv`.
