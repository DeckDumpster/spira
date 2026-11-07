# inbox-triage — design

Replaces `spira/inbox-triage.sh` (32 lines) with one Rust binary, `inbox-triage`, invoked by
bare name from the release PATH. Bead: sp-48f6g (rewrite wave 5b).

## Intent

The concierge's mandatory first action at every session start is arming this as a Monitor
(`spira/hooks/session.sh`): it tails `SPIRA_CONCIERGE_INBOX` — the durable log every watcher
and `inbox-append.sh`/`spira-mail-deliver` append to — and is how events reach the session
with no keystroke into the pane (operator, 2026-09-27: nothing types into the pane). It must
stay constantly attached; `inbox-keeper.sh` (a separate watcher, unchanged by this bead)
re-arms it if it lapses.

It applies three cheap heuristics to each new line, unchanged from the bash:

* **DROP** echoes of the concierge's own actions and purely informational events (`ROUND
  RESULT`, ` OPENED `, `pool: NEW CERTIFIED`).
* **DEDUP** the same text within `SPIRA_CONCIERGE_INBOX_DEDUP` seconds (default 600) — the
  dedup KEY strips the leading timestamp and any `HH:MM:SS(Z?)` substring, and normalises
  `oldest <n>s` to `oldest Ns`, so the same standing condition reported with a different
  elapsed time still dedupes.
* **PASS** everything else, printed to stdout with the leading timestamp stripped.

## Non-goals

* `conf.sh`, `inbox-append.sh`, `inbox-keeper.sh`, `spira-mail-deliver.sh`, `hooks/session.sh`
  — all stay bash; only the call sites that name `inbox-triage.sh` move to the bare binary
  name in this change.
* The log's format or location (`$SPIRA_RUN/watchd/concierge-inbox.log` by default,
  `SPIRA_CONCIERGE_INBOX`). Unchanged.

## Contract

`inbox-triage` (no arguments). Reads `SPIRA_CONCIERGE_INBOX` and
`SPIRA_CONCIERGE_INBOX_DEDUP` through the `conf.sh` seam (same pattern as `watchd`,
DESIGN-in-`../watchd/DESIGN.md`). Creates the log if it does not exist (refuses if it
cannot). Starts reading from the CURRENT end of the file (`tail -n 0 -F`'s behaviour — this
is a live Monitor, not a replay tool; a session wanting history reads the file directly).
Runs until killed; every shown line is flushed immediately.

### An operator-side fork may exist outside this repository

An installation may keep a hand-maintained copy of the old `inbox-triage.sh` somewhere in its
own runtime state (outside this repository), hardcoding the log path and the dedup window
rather than sourcing `conf.sh` — predating this rewrite, and possibly what a live Monitor is
running at the moment this lands. This bead does not touch any such copy (outside the harness
repository, and a live process's script while this bead is delivered). The operator-side
change this rewrite makes necessary: repoint that copy at the `inbox-triage` binary (or retire
it in favour of the binary directly, now that the binary takes no arguments and behaves
identically for the default 600 s window) at the next convenient re-arm. Reported, not done
here (DESIGN.md is not production, and rearming a live Monitor for another session is out of
this bead's reach) — see the delivery report for the specific case found at review time.

## Decisions

* **Dedup key normalisation uses `regex`** (already a workspace dependency) rather than
  hand-rolled scanning, for the same two substitutions the bash's `sed -E` made:
  `[0-9]{2}:[0-9]{2}:[0-9]{2}Z?` and `oldest [0-9]+s`.
* **No inotify crate is vendored** in this workspace's registry cache (DESIGN.md precedent:
  `watchd/DESIGN.md` "Decisions"); a 200 ms poll meets a Monitor's latency need.

## `inbox-triage append <text>`

Replaces the body of `spira/inbox-append.sh`, which becomes `exec inbox-triage append "$*"`
(every caller keeps the name, the arguments and exit 0).

* Appends `<ISO-8601 Z> <text>` to `SPIRA_CONCIERGE_INBOX`, creating the parent directory;
  never truncates.
* If `SPIRA_CONCIERGE_PUSH_WAKE` is non-empty and the text matches
  `SPIRA_CONCIERGE_PUSH_PATTERN`, the same line is also appended to `$SPIRA_CONCIERGE_INBOX.push`
  and a single detached flusher is started; `append` returns at once, exit 0.
* The flusher takes a non-blocking lock on `$SPIRA_CONCIERGE_INBOX.push.lock` (a holder already
  draining means it exits) and drains the spool until it is gone. A batch is due when the spool
  size has not changed for `SPIRA_CONCIERGE_PUSH_IDLE` seconds or `SPIRA_CONCIERGE_PUSH_WINDOW`
  seconds have passed (five ticks a second). It then runs `$SPIRA_CONCIERGE_PUSH_WAKE
  "[inbox-push] <n> urgent event(s): <lines, each followed by ;>"`.
* `inbox-triage flush` is the flusher's own entry (spawned by `append`, not for callers).
* `SPIRA_CONCIERGE_PUSH_PATTERN` is matched with the `regex` crate, as `watchd` already does, where
  the script used `grep -E`; a pattern it rejects pushes nothing.
* The timing rule is the pure `inbox_rules::Flush` (time is its `tick` count), the line, urgency
  and wake text are `inbox_rules::{line, is_urgent, wake_text}`; only the binary sleeps.

## Test strategy

Unit: timestamp stripping, the three DROP patterns, the dedup key normalisation, and the
dedup window itself (inject a clock); the coalescer by tick count; `append_line` against a
tempdir. The live Monitor loop against a growing file is not covered by a suite.
