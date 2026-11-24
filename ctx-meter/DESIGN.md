# ctx-meter

How much context a session carries, how fast it grows, how far the archivist has got, and how
full the account's two rate-limit windows are. One refresh is **one process that starts no
other** (`tests/no_spawn.rs` scans `src/` for every spelling of a spawn, with a planted control).
Replaces `spira/ctx-meter.sh`, whose refresh started about twenty programs.

The status line and the cockpit's TOKENS pane ask one question and must not answer it twice,
so the measurement lives once, in the library: the binary renders it for the client, and the
archivist's sweep (`archivist::seam::ctx_meter_env`) calls it in process.

## Arguments and exit codes

| Invocation | Reads | Prints | Writes |
|---|---|---|---|
| `ctx-meter` | the client's status-line JSON on stdin | one coloured line | pointer, cursor, limits samples |
| `ctx-meter env [<transcript>]` | no stdin, ever | `SP_CTX_*` then `SP_LIMIT_*` as `KEY=value` lines | transcript cursor |

`env` names a transcript when the caller has one (the archivist walks every live session);
without one it answers for the session the operator pointer names, and prints `-` when the
pointer is absent, stale, corrupt or points at a deleted file.

Exit codes: `0` printed; `1` configuration refused (a missing or unparsable key, named in the
message, `SPIRA_ARCHIVIST_IDLE` included, or no resolvable `SPIRA_TOML`); `2` usage. A line or
a block is never printed on a refusal: a gauge that cannot read says so on stderr and exits
non-zero rather than printing a plausible number.

`SPIRA_NOW` (epoch seconds) replaces the clock for a caller that exports it on purpose; it is
not a config key. The core takes `now`, the directories and the thresholds as arguments, so
`env`'s `SP_CTX_AGE` for a named transcript with no pointer is `now` minus its mtime (the shell
script read the wall clock there; the two differ only under `SPIRA_NOW`).

The start count is a measured fact, not an assertion of the tests: `strace -f -e trace=execve`
of one refresh shows one `execve`. What the tests hold is the source: `tests/no_spawn.rs`.

## Configuration

Each key is read with `spira_config::process::cfg`, one at a time: `SPIRA_CTX_WARN`,
`SPIRA_CTX_HIGH`, `SPIRA_CTX_LIMIT`, `SPIRA_TOKEN_PROJECTS`, `SPIRA_RUN`,
`SPIRA_ARCHIVIST_IDLE`. `config()` and `run_dir()` are avoided: they start `git` to derive the
home repository, which is a process per refresh. Run metrics go out through
`spira_otel::oneshot` (a push from this process, never a child).

## The line

`▁ ctx 123k /25t +5.0k/turn <archivist> <windows> <in flight>`

- **Headline** is the hook's `context_window.total_input_tokens` when `current_usage` is
  non-null, else the transcript's last turn. A named session with no turn yet is `ctx fresh`;
  a gauge that cannot read is `ctx ?`. Neither is zero.
- **Turns and growth** come from the transcript, read forward from a cursor. Growth needs 21
  turns (the last 20 deltas averaged).
- **Archivist** renders `$SPIRA_RUN/archivist/<session>.state` (`state`, `at_turn`,
  `items_filed`, `retry_at`): sweeping, archiving, safe (demoted to "safe as of Nt ago" after
  two turns), failed/retry with the local time of the retry, timeout, capacity, none (quiet
  below the warn line, `not archived` above it, `skipped (capacity)` when the last sweep was
  skipped), and `?` for an unreadable file.
- **Windows** render, per window and in the fixed order 5-hour then 7-day, when it clears and
  how full it is; a projection (`↑rate full in ~T`) appears only when the window fills before it
  resets. A window the client did not send renders nothing.
- **In flight** is the collector's `SP_INFLIGHT_SEG` from `cockpit.env`; missing, empty or
  older than 60 s is a red `?inflight`, never idle.

## Files under `SPIRA_RUN`

| File | Format | Meaning |
|---|---|---|
| `ctx-operator.ptr` | `tp=`, `ts=`, `sid=` lines | The operator's transcript. Written by each line run unless another live holder spoke more recently (transcript mtime); a holder older than `SPIRA_ARCHIVIST_IDLE` is never deferred to. |
| `limits.samples` | `<epoch> <pct> <resets_at>` ×2 (`- -` for an absent window) | Account-wide samples, six hours kept, a run of equal readings kept as its two endpoints. One unparsable line refuses the whole file (`?` in `env`); the next hook in hand replaces it. |
| `ctx-meter/<digest>.cursor` | `path`, `ino`, `head`, `off`, `turns`, `last`, `tail` | Where the last run stopped. Keyed on path, inode and a digest of the first 256 bytes; any disagreement, or an offset past the end, is a full read. The digests are `sha2` (the shell script's were `sha1`: an old cursor costs one full read). |
| `archivist/<session>.state`, `archivist/sweep.state` | `key=value` | Written by the archivist; read here. |
| `cockpit.env` | shell `KEY='value'` | Written by the collector; only `SP_INFLIGHT_SEG` is read. |

Every file written here is written whole to a pid-named sibling and renamed.

## Rates

A burn rate is the rise across the retained span since the window's last reset (a drop of more
than a point), never a pairwise slope: the percentage is rounded to a point, so a pairwise
slope is seeded with the rounding step. It needs 10 minutes of span and 2 points of rise.

## Tests

`src/tests.rs` holds the cases of the retired shell suites on their fixtures, with expected
strings captured from `ctx-meter.sh` before it was deleted; `tests/as_client.rs` runs the
registered status-line command the way the client runs it.
