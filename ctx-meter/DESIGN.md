# ctx-meter

How much context a session carries, how fast it grows, how far the archivist has got, and how
full the account's two rate-limit windows are. One refresh is **one process that starts no
other** (`tests/no_spawn.rs` scans `src/` for every spelling of a spawn, with a planted control).
Replaces `spira/ctx-meter.sh`, whose refresh started about twenty programs.

The status line and the cockpit's TOKENS pane ask one question and must not answer it twice,
so the measurement lives once, in the library: the scheduler's `ctx.status` source computes it
(`status::compute`), the archivist's sweep calls `env` in process.

## Arguments and exit codes

| Invocation | Reads | Prints | Writes |
|---|---|---|---|
| `ctx-meter line --run <dir> --metrics <endpoint>` | the client's status-line JSON on stdin, the session's status file, the limits samples | one coloured line | the session record, limits samples |
| `ctx-meter env [--run <dir>] [<transcript>]` | no stdin, ever | `SP_CTX_*` then `SP_LIMIT_*` as `KEY=value` lines | transcript cursor |

`line` reads no transcript and no configuration, and both flags are required: without either it
exits 2. `<dir>` is `spira_config::ctx_meter_dir` of the user runtime directory (memory-backed;
a reboot empties it and the source rewrites every session at start); `session_hook` writes it
into the registered command. `env` defaults `--run` to that directory under `$XDG_RUNTIME_DIR`.

`env` names a transcript when the caller has one (the archivist walks every live session);
without one it answers for the live session record with the newest transcript (a record touched
within `SPIRA_ARCHIVIST_IDLE`), and prints `-` when there is none.

Exit codes: `0` printed; `1` `env`'s configuration refused (a missing or unparsable key, named in
the message, or no resolvable `SPIRA_TOML`); `2` usage.

`SPIRA_NOW` (epoch seconds) replaces the clock for a caller that exports it on purpose; it is
not a config key.

The start count is a measured fact, not an assertion of the tests: `strace -f -e trace=execve`
of one refresh shows one `execve`. What the tests hold is the source: `tests/no_spawn.rs`.

## Configuration

`env` reads each key with `spira_config::process::cfg`, one at a time: `SPIRA_CTX_WARN`,
`SPIRA_CTX_HIGH`, `SPIRA_CTX_LIMIT`, `SPIRA_TOKEN_PROJECTS`, `SPIRA_RUN`,
`SPIRA_ARCHIVIST_IDLE`; the `ctx.status` source pins the same keys. `config()` and `run_dir()`
are avoided: they start `git` to derive the home repository, which is a process per call. Run
metrics go out through `spira_otel::oneshot` to the `--metrics` endpoint (a push from this
process, never a child).

## The line

`▁ ctx 123k /25t +5.0k/turn <archivist> <windows> <in flight>`

The client's figures (`context_window`, `rate_limits`) are read from stdin; everything else is the
status file's `thresholds`, `turns`, `growth`, `ctx`, two archivist segments (below and at or above
the warn line) and the in-flight segment, each rendered when the file was written.

- **Headline** is the hook's `context_window.total_input_tokens` when `current_usage` is
  non-null, else the status file's last turn. A session with no turn yet is `ctx fresh`; a
  transcript that cannot be read is `ctx ?`. Neither is zero.
- **Without a usable status** (no valid session id, no file, or one untouched for 60 s) the line is
  the client's figures alone, `ctx ?` when it sent none, then the windows and a red `?inflight`.
- **Turns and growth** come from the transcript, read forward from a cursor by the source. Growth
  needs 21 turns (the last 20 deltas averaged).
- **Archivist** renders `$SPIRA_RUN/archivist/<session>.state`: sweeping, archiving, safe (demoted
  to "safe as of Nt ago" after two turns), failed/retry with the local time of the retry, timeout,
  capacity, none (quiet below the warn line, `not archived` above it, `skipped (capacity)` when
  `sweep.state` says the last sweep was skipped), and `?` for an unreadable file.
- **Windows** render, per window and in the fixed order 5-hour then 7-day, when it clears and
  how full it is; a projection (`↑rate full in ~T`) appears only when the window fills before it
  resets. A window the client did not send renders nothing.
- **In flight** is the collector's `SP_INFLIGHT_SEG`, set by `cockpit-collect`'s tier publish;
  missing or empty is a red `?inflight`, never idle.

## The records

All JSON, under `<dir>`:

| File | Writer | Meaning |
|---|---|---|
| `sessions/<sid>.json` | the refresh | `session`, `transcript`, `cwd`, `len` (one `stat` of the transcript, so growth shows one refresh late). Unchanged, its mtime is set through a read-only open, which raises no watched event; this is the liveness `env` and the prune read. |
| `status/<sid>.json` | the scheduler, under `status::lock` | the figures above. Replaced by rename only on change; otherwise touched, by the source or by the tier publish. |
| `limits.samples` | the refresh | `<epoch> <pct> <resets_at>` ×2 (`- -` for an absent window), six hours kept, a run of equal readings kept as its two endpoints. One unparsable line refuses the whole file (`?` in `env`); the next hook in hand replaces it. |

The source (`spira-runtime/src/sources/ctx_status.rs`) recomputes one session when its record or
`<sid>.state` is written, all of them when `sweep.state` is written and at start, and prunes
records untouched past `SPIRA_ARCHIVIST_IDLE` with their status. It writes only where
`SPIRA_RUNTIME_COCKPIT_TIER_WRITE` is true.

Other files under `SPIRA_RUN`:

| File | Format | Meaning |
|---|---|---|
| `ctx-meter/<digest>.cursor` | `path`, `ino`, `head`, `off`, `turns`, `last`, `tail` | Where the last scan stopped. Keyed on path, inode and a digest of the first 256 bytes; any disagreement, or an offset past the end, is a full read. |
| `archivist/<session>.state`, `archivist/sweep.state` | `key=value` | Written by the archivist; read by the source. |
| `cockpit.env` | shell `KEY='value'` | Written by the collector; only `SP_INFLIGHT_SEG` is read, once, when a status is first made. |

Every file written here is written whole to a pid-named sibling and renamed.

## Rates

A burn rate is the rise across the retained span since the window's last reset (a drop of more
than a point), never a pairwise slope: the percentage is rounded to a point, so a pairwise
slope is seeded with the rounding step. It needs 10 minutes of span and 2 points of rise.

## Tests

`src/tests.rs` holds the cases of the retired shell suites on their fixtures, with expected
strings captured from `ctx-meter.sh` before it was deleted, each run through the record, the
source's recompute and the refresh; `tests/as_client.rs` runs the
registered status-line command the way the client runs it.
