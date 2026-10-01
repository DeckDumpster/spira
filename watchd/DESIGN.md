# watchd — design

Replaces `spira/watchd.sh` (1,737 lines) with one Rust binary, `watchd`, invoked by bare name
from the release PATH (law-new-subsystems-are-rust, sp-gypjk). Bead: sp-48f6g (rewrite wave 5b,
`wiki/projects/spira/remaining-bash-inventory.md` group 5, "operator surface").

## Intent

`watchd` is the face over the logs and cursors systemd fills. It is the one place that knows:

* what a watcher IS (the manifest: `name|kind|target|health`, one row per watcher, plus an
  operator overlay);
* what a reader has already seen (`<name>.cursor`, a line count);
* whether a watcher can still see what it watches (a health probe, run only from `status`);
* who currently holds the one legitimate stream on a watcher's log (`<name>.tail.lock`, a
  kernel `flock`, never a pid file);
* when a standing backlog or a dead watcher has waited long enough that a session boundary is
  not enough, and a headless channel (mail) has to be told (`notify`, for a timer).

Systemd owns every process; `watchd` decides nothing about whether one is running and starts
nothing except when it IS the process (`exec`, what `ExecStart` calls). Two files per watcher —
`<name>.log`, `<name>.cursor` — are the entire contract a reader depends on; every subcommand
below is arithmetic over those two files and the manifest, which is what keeps the contract
agent-agnostic: `tail -n +$((cursor+1)) -F <log>` remains a conforming client even though the
canonical reader is now this binary.

## Non-goals

* **Touching `conf.sh` or `lib.sh`.** Both stay bash until the config/store-core group of the
  rewrite order (group 4, "leave lib.sh alone" — Ryan's standing instruction during the
  cutover). `watchd` reads the values it needs from the *environment conf.sh has already
  exported* into its own process, via a one-shot subshell ("the seam", `context.rs`) — the same
  pattern `gate` and `queue-watch` already use for `lib.sh`. No config-resolution logic is
  reimplemented here.
* **`mail`, `mail-health.sh`.** `mail` was rewritten and retired from bash by sp-ooh1k;
  `mail-health.sh` stays bash (a different rewrite wave). `watchd` execs both exactly as the
  bash execed `mail.sh`/`mail-health.sh`: `mail send …` for an escalation, `mail-health.sh`
  as the second half of `notify`.
* **`watch-refresh.sh`, `doctor.sh`, `install.sh`, `units.sh`, `cockpit/rebuild.sh`,
  `cockpit/remote/cockpit-remote`.** These call `watchd`; only their call sites move to the
  new bare name in this change, not their own logic.
* **The manifest FORMAT.** `name|kind|target[|health]`, the `@KEY@` placeholder syntax, the
  leading-`?` optional-row marker: unchanged, because every operator-written row in
  `spira/watchers` and `~/.config/spira/watchers.d/*.watchers` has to keep parsing.

## Contract

### Invocation

```
watchd manifest                every valid row, expanded: name|kind|target|health
watchd units                   the unit name of every `daemon` row, one per line
watchd keys                    the placeholders a row may name, one per line
watchd exec <name>              become that watcher; this is what ExecStart calls
watchd status                  a table, one line per watcher, then a DEGRADED/NOT INSTALLED block
watchd drain [name] [--all]    print what nobody has read, and mark it read
watchd peek [name] [--all] [--limit N]
                                the same, capped, marking NOTHING read
watchd tail <name> [--all] [--takeover] [--from-start]
                                replay from the cursor, then stream; for a Monitor
watchd tailers                 name|pid|since for every watcher being tailed now
watchd restart [name]          restart the unit behind a watcher
watchd notify                  escalate events nobody has drained, and watchers unwell too long
watchd prune                   remove lock/cursor/pending files and retired units
watchd health-ids <file>       assert a state file names at least one of our own beads
watchd health-view <prog> <session>
                                assert the view a follower steers matches the one it wants
```

Argument parsing, exit codes (0 pass, 1 finding/fault, 2 usage/argument, 3 could-not-check —
`notify` only), and every message text are unchanged from the bash where a caller or a test
depends on the literal string — see `src/cli.rs` and the parity fixtures in
`tests/parity.rs`.

### The manifest

`SPIRA_WATCHERS` (default `$SPIRA_HOME/watchers`) plus every `*.watchers` file in
`SPIRA_WATCHERS_OVERLAY` (default `$XDG_CONFIG_HOME/spira/watchers.d`, only if the directory
exists), parsed by the SAME rule for both: pipe-delimited `name|kind|target[|health]`, `#`
comments, blank lines skipped, a leading `?` on the name marks the row optional. **One
malformed line refuses the whole manifest** (harness rows and overlay rows alike) — a parser
that skipped the bad row and reported the rest would let a typo in one file silently run every
other watcher while claiming nothing was wrong (law-absence-needs-a-positive-control). An
optional row whose placeholder key is unset becomes kind `off` rather than being dropped, so
`status` can say "this watcher exists and is not configured" instead of going silent about it.

Kinds: `daemon` (we run it, via `exec`), `log` (something else writes it, the target is the log
path itself), `extern` (an existing unit we only monitor), `off` (optional, not configured).

Placeholders (`WATCHD_KEYS`, unchanged): `SPIRA_HOME SPIRA_REPO SPIRA_RUN SPIRA_COCKPIT
SPIRA_DB SPIRA_WORKSPACES SPIRA_TOWN SPIRA_WIKI SPIRA_VIEW SPIRA_VIEW_SESSION`.

### Environment (read once, through the conf.sh seam)

| variable | use | default (conf.sh) |
|---|---|---|
| `SPIRA_RUN` | `watchd/` lives under it | — |
| `SPIRA_HOME`, the ten `WATCHD_KEYS` | `@KEY@` expansion | — |
| `SPIRA_WATCHERS`, `SPIRA_WATCHERS_OVERLAY` | manifest files | `$SPIRA_HOME/watchers`, `~/.config/spira/watchers.d` |
| `SPIRA_CONF_FILE` | named in refusal messages | — |
| `SPIRA_ACTIONABLE` | the default filter regex (`drain`/`tail`/`notify`) | a fixed alternation |
| `SPIRA_HEALTH_TIMEOUT` | bound on a health probe, seconds | 10 |
| `SPIRA_NOTIFY_AGE` | backlog/unhealthy age before `notify` escalates, seconds | 1800 |
| `SPIRA_INSTANCE` | unit-name suffix (`watch_unit_name`) | `prod` |
| `SPIRA_SYSTEMCTL` | override for `systemctl` (tests) | `systemctl` |
| `SPIRA_BD`, `SPIRA_DB`, `SPIRA_ID_PREFIX` | `health-ids`' own-prefix derivation | — |
| `SPIRA_NOW` | frozen clock for tests | wall clock |

`watchd` refuses (NO_VERDICT-shaped: prints why, exits non-zero) when the seam itself cannot
run — a missing or unsourceable `conf.sh` is a machinery fault, not a finding about any
watcher.

### Files under `$SPIRA_RUN/watchd/`

Unchanged: `<name>.log` (daemon target's stdout, or the `log`-kind target itself),
`<name>.cursor` (integer, delivered-through count), `<name>.tail.lock` (the flock — no pid
file decides anything; a pid is recorded inside it for the message only), `<name>.pending`
(`<line> <epoch>` — the backlog clock), `<name>.restarts`, `<name>.unhealthy`.

## Decisions

Accreted bash workarounds this port drops, because a single native process does not need them
(law-rust-rewrites-start-from-intent: name what is dropped, never silently carry it):

* **The mawk/gawk `fflush()` detection (`_wd_stream_awk`).** Existed only because `tail -F |
  awk` needed a second process to line-buffer its output on Debian/Ubuntu's default `awk`.
  `watchd tail` is one process reading and writing directly; there is no pipe stage to buffer.
* **The external `timeout` binary, with its "not on PATH" unbounded fallback (`_wd_probe`).**
  A health probe's bound is now enforced by the process itself (spawn, poll for exit with a
  deadline, kill on timeout) — no dependency on a `timeout` executable existing, and no silent
  "unbounded" mode to fall into.
* **The background-pipeline-plus-trap-plus-pidfile dance in `cmd_tail`/`_wd_tail_stop`.**
  Existed because the bash `tail` command owned the read loop and `watchd.sh` could only
  supervise it from outside. `watchd tail` reads the file itself, so TERM/INT/HUP are handled
  in-process and the loop simply stops; there are no child processes to track pids for or to
  kill on the way out.
* **Polling replaces `tail -F`'s inotify.** No filesystem-notification crate is vendored in
  this workspace's registry cache; a 200 ms poll (read-to-EOF, sleep, repeat) meets the same
  latency a human or a Monitor needs and is bounded, unlike an unbounded wait. Truncation is
  detected by size and handled the way `tail -F` handles it: re-read from the start.

Everything else — the exact-range `sed`-equivalent read (never `tail | head`, because the log
is being appended to while it runs), the clamped cursor, the reader-lock-before-cursor-read
ordering, the takeover protocol (TERM the incumbent, wait up to 10 s for the lock, never guess
at staleness), the backlog clock keyed on the OLDEST unread line's position rather than a
count, the escalation fingerprint (ask once per distinct backlog, not once per pass), and the
HALTED/DEGRADED/off three-way split in `status` — is behaviour, not accident, and is ported
unchanged with a unit test asserting it.

## Test strategy

* **Unit** (no filesystem, no subprocess): manifest parsing (valid rows, every fault case,
  optional-row-to-`off`, one bad line refusing the whole file, the overlay merge), cursor
  clamping, age formatting, the default `SPIRA_ACTIONABLE` filter, the escalation fingerprint.
* **Unit with a real temp directory** (`testkit::TempDir`, no subprocess): the exact-range
  read, cursor read/write, prune's file selection, the lock's single-reader guarantee (two
  threads racing `flock`).
* **Unit with a fake `Ops`** (`src/ops.rs` trait: `is_active`, `unit_show`, `restart`,
  `mail_ask`, `orphan_lock`): `status`'s three-column logic, `notify`'s two-half behaviour
  (backlog vs. dead watcher), `restart`'s all-vs-one framing — without a systemd user manager
  or a mailbox.
* **Cannot be verified without a real systemd/tmux/mail**: exercised instead by parity runs
  against the bash on this box (below) and left to the round's integration tier per
  `gate-unit-round-integration-2026-09-29`.
