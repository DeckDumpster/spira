# inbox-triage — design

Replaces `spira/inbox-triage.sh` (32 lines) with one Rust binary, `inbox-triage`, invoked by
bare name from the release PATH. Bead: sp-48f6g (rewrite wave 5b).

## Intent

The concierge's mandatory first action at every session start is arming this as a Monitor
(`spira/hooks/session.sh`): it tails `SPIRA_CONCIERGE_INBOX` — the durable log every watcher
and `inbox-append.sh`/`spira-mail-deliver` append to — and is how events reach the session
with no keystroke into the pane (per Ryan, 2026-09-27: nothing types into the pane). It must
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

### An operator-side fork exists outside this repository

`/home/ryan/spira/run/concierge-notes/inbox-triage.sh` is a hand-maintained copy that
hardcodes the log path and the 600 s dedup window rather than sourcing `conf.sh` — it predates
this rewrite and is what the brain Concierge's own live session Monitor runs today. This bead
does not touch it (it is outside the harness repository and is a live process's script while
this bead is delivered). The operator-side change this rewrite makes necessary: repoint that
copy at the `inbox-triage` binary (or retire it in favour of the binary directly, now that the
binary takes no arguments and behaves identically for the default 600 s window) at the next
convenient re-arm. Reported, not done here (DESIGN.md is not production, and rearming a live
Monitor for another session is out of this bead's reach).

## Decisions

* **Dedup key normalisation uses `regex`** (already a workspace dependency) rather than
  hand-rolled scanning, for the same two substitutions the bash's `sed -E` made:
  `[0-9]{2}:[0-9]{2}:[0-9]{2}Z?` and `oldest [0-9]+s`.
* **No inotify crate is vendored** in this workspace's registry cache (DESIGN.md precedent:
  `watchd/DESIGN.md` "Decisions"); a 200 ms poll meets a Monitor's latency need.

## Test strategy

Unit: timestamp stripping, the three DROP patterns, the dedup key normalisation, and the
dedup window itself (inject a clock). Not verified here: the live Monitor loop against a real
`tail -F`-style growing file — covered by running the binary in the same way
`test-concierge-inbox.sh` exercised the bash (repointed, not retired, since it is the only
place this binary's actual streaming behaviour is exercised end to end).
