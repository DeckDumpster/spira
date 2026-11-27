# inbox-triage — design

Replaces `spira/inbox-triage.sh` (32 lines) with one Rust binary, `inbox-triage`, invoked by
bare name from the release PATH. Bead: sp-48f6g (rewrite wave 5b).

## Intent

It tails `SPIRA_CONCIERGE_INBOX` — the durable log every watcher and
`inbox-append.sh`/`spira-mail-deliver` append to — with no keystroke into the pane
(operator, 2026-09-27: nothing types into the pane). The Concierge session's event reader is
`spira-sub follow` (`spira/hooks/session.sh`).

It applies three cheap heuristics to each new line, unchanged from the bash:

* **DROP** echoes of the concierge's own actions and purely informational events (`ROUND
  RESULT`, ` OPENED `, `pool: NEW CERTIFIED`).
* **DEDUP** the same text within `SPIRA_CONCIERGE_INBOX_DEDUP` seconds (default 600) — the
  dedup KEY strips the leading timestamp and any `HH:MM:SS(Z?)` substring, and normalises
  `oldest <n>s` to `oldest Ns`, so the same standing condition reported with a different
  elapsed time still dedupes.
* **PASS** everything else, printed to stdout with the leading timestamp stripped.

## Non-goals

* `conf.sh`, `inbox-append.sh`, `spira-mail-deliver.sh`, `hooks/session.sh`
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
* Publishes `concierge/inbox` with the unread count. An urgent line is recorded and announced
  like every other: nothing spools it or runs a command.
* **One write leg for every writer (sp-nasmjd).** Appending the line and announcing it are
  `inbox_rules::record`, a library function with no store and no scheduler behind it. `append`,
  spira-runtime's `World::inbox_append` and heimdall's page call it. Every other writer holds an
  `inbox_rules::Sink`, built by `Sink::configured(<publisher name>)` from
  `SPIRA_CONCIERGE_INBOX` and `SPIRA_RUN`, and calls `Sink::record`. It fails only when the line
  did not reach the inbox. A Rust writer of `SPIRA_CONCIERGE_INBOX` never writes the file itself:
  a direct write is never announced on `concierge/inbox`.
* Any argument other than `append` is refused as an unknown verb (exit 2).

## Test strategy

Unit: timestamp stripping, the three DROP patterns, the dedup key normalisation, and the
dedup window itself (inject a clock); `append_line` against a
tempdir. The live Monitor loop against a growing file is not covered by a suite.
