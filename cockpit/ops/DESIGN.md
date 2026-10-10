# cockpit-ops — the ops dashboard pane, its tmux layout, and the bead-answer plumbing

Replaces `cockpit/health.sh` (1,951 lines), `cockpit/layout.sh` (725), `cockpit/rebuild.sh`
(363), `cockpit/resolve.sh` (49) and `cockpit/reply.sh` (46) — sp-llbmi, rewrite wave 5d. One
crate, five binaries (`health`, `layout`, `rebuild`, `resolve`, `reply`), because the five
scripts already shared one concern — the cockpit that is on screen right now — and a crate
per script would recreate the five-copies-of-one-idea problem `cockpit/db.sh`'s own header
names as the reason it is one function and not five.

## 1. Intent

The cockpit is Ryan's live operating surface: a tmux layout with the session pane top-left,
mail bottom-left, and a read-only ops dashboard (`health`) taking the full right column.
`layout` builds and self-heals that arrangement; `rebuild` assembles it from nothing after
the tmux server itself dies; `resolve`/`reply` are how an agent closes or answers a bead in
its own thread instead of forking the conversation into chat or a new bead.

Same subcommands, same output contract, same tag-based pane addressing as the bash
originals — this document is the contract; the code satisfies it and the unit tests are
derived from it.

## 2. Contract

### 2.1 `health [once [rows [cols]]|render-many <dir> [rows [cols]]|loop]`

- `loop` (default, no args): repaint every `$SPIRA_HEALTH_TICK` seconds (default 2) forever.
  Never clears the screen — homes the cursor and erases line-by-line so no blank frame is
  ever visible; skips the write entirely when the frame is unchanged from the last tick.
- `once [rows [cols]]`: one frame, sized for a pane `rows` tall and `cols` wide (0 = use the
  real terminal size via `stty size`, falling back to `$LINES`/`$COLUMNS`, falling back to a
  deliberately pessimistic 5×80). Exits after printing.
- `render-many <dir> [rows [cols]]`: one frame per `*.env` fragment in `<dir>` (sorted by
  filename), each preceded by `=== <name> ===` — the fixture-driven-suite seam.
- Never calls `bd` or `git` at runtime on the repaint path (the one exception, matching the
  bash original: a `git rev-parse --short HEAD` for the renderer-revision banner, done once
  at process start, never per tick).
- Reads `$SPIRA_RUN/cockpit.env`, written by `spira/cockpit.sh` (sp-kt4l3, concurrent — not
  this bead). A missing/stale/malformed value renders `?`, never `0` or a blank
  (law-absence-needs-a-positive-control) — every section function is unit tested against
  both a present and an absent/`?` reading.

- ROUNDS section (`health/round.rs`): every live (non-terminal) batch from the collector's `round`
  probe, which reads `spira-lc list --batches` and the bead rows for the pool — no `bd`, no logs.
  Keys are `SP_ROUNDS_N` and `SP_ROUNDS<i>_{NAME,PHASE,OPENED,N,MEMBER<j>,EJECT_N,EJECT<j>}`; at
  most four are listed, the rest counted. Each round is one collapsed row (name, phase, member
  count, ejection count); the round certifying on the VM expands to its members and ejections
  when the pane has room. Landed and abandoned rounds leave the section. Wall time is computed at
  render from `SP_ROUNDS<i>_OPENED` against `SP_ROUND_CAP` (`SPIRA_ROUND_CERTIFY_WALL_SECS`):
  amber past 80%, red past the cap. With no live batch: last verdict and the SUBMITTED +
  CERTIFIED unheld pool. Under 60 columns, one line per round.

### 2.2 `layout up|down|status|ensure [--window <target>]`

- `up`: create-or-repair the dashboard in `<target>` (default: the window this process is
  running in, else the first pane running `claude`, else `cockpit:2`). Idempotent: kills and
  re-splits every tagged dashboard pane, leaves the session pane alone, restores it as the
  active pane on exit.
- `down`: remove both dashboard panes, leaving the session pane full-height. Refuses if no
  session pane can be found (never empties the window to zero panes). Records a deliberate
  marker file (`$SPIRA_RUN/cockpit.down`) so `ensure` can tell "torn down on purpose" apart
  from "the whole window crashed" — both leave zero `@cockpit`-tagged panes anywhere.
- `status`: one-line report of what is running; changes nothing.
- `ensure`: heal every window already carrying a cockpit, **silently** when nothing needs it.
  Refuses (exit 0, not an error) when the running binary is not the one installed at
  `$SPIRA_RELEASE/bin/layout` — a worktree copy must never adopt and respawn the operator's
  live panes with its own paths. Calls `rebuild` (bare name, by way of `$PATH`) only when
  **zero** `@cockpit`-tagged panes exist anywhere AND no down-marker is present AND a 60s
  (`$COCKPIT_HEAL_COOLDOWN`) repair cooldown has elapsed.
- Every pane is addressed by its tmux pane-scoped option `@cockpit` (`health`/`mail`), read
  by `#{pane_id}`, **never by index** — tmux renumbers indices the instant a pane dies, and
  the session pane is the one most likely to.
- Identity is re-derived every pass from what a pane is actually **running** (its own argv,
  or its children's, matched by argv **position** — `exe == ".../health"` or, for a shell,
  `argv[1]` ends in `.sh`/the bare name), never from tmux's `pane_start_command` (survives
  `respawn-pane`, so a swapped pane keeps a stale tag) and never as a substring of the whole
  command line (a session merely *mentioning* `cockpit/health.sh` in a system prompt must not
  be classified `health` — a named scar).

### 2.3 `rebuild [probe|--probe] [--force] [-h|--help]`

- `probe`: report server/session/dashboard state; changes nothing.
- (bare): clear a wedged server, create any missing session, build the dashboards into
  `brain:0` (never a fresh `cockpit` window — see §4), link `brain:0`/`hunk:0` into `cockpit`
  via `cockpit-remote`, then verify with a battery of **positive-control** checks (an empty
  pane capture is a FAIL, not a pass).
- `--force`: also clear a wedged server that still holds live descendant processes (otherwise
  refused — "wedged" is not "dead"; a server with live panes under it is somebody's unsaved
  work, not a corpse).
- The server's holder PID is found by the **listening socket's inode** in `/proc/net/unix`,
  never by `pgrep -f`/`pkill -f` (a named scar: a pattern match once killed the shell that
  invoked it).

### 2.4 `resolve <bead-id> "<reason>"` / `resolve <bead-id> -` (reason on stdin)

Closes the bead as actor `claude`, `--force` (a verdict on an ask, not a work reassignment;
rig beads are usually assigned to the mayor). `bd`'s stdout+stderr are both captured and
shown on failure — `bd` exits 0 on some refusals, printing the complaint to stdout. Exit 0 on
success, 1 on a `bd` failure or an unresolvable database, 2 on a usage error.

### 2.5 `reply <bead-id> "<text>"` / `reply <bead-id> -` (text on stdin)

Comments on the bead as actor `claude`, distinct from `$SPIRA_OPERATOR_ACTOR` so the
answer-watcher can tell an agent's reply from the operator's own. Same exit codes as
`resolve`.

### 2.6 The database (`db.rs`)

One function, not five copies: `$COCKPIT_DB`, falling back to `$SPIRA_DB`, verified to hold a
`.beads` directory — refusing to guess when it does not. `cockpit/db.sh` itself is **not**
deleted: `cockpit/moot-sweep.sh` and `cockpit/verify-asks.sh` still source it and are outside
this bead's scope (bash-only callers keep the bash function until they, too, move).

## 3. Schema

`health` reads `$SPIRA_RUN/cockpit.env`: lines of `KEY='value'`, single-quoted with bash's own
escaping (`'` → `'\''`) by `cockpit.sh`'s `write_snapshot` before the file ever reaches disk —
parsed directly (`health::model::Snapshot`), never by shelling out to `bash -c 'source ...'`,
since a repaint every 2s cannot afford a fork. First occurrence of a key wins, matching the
writer's own dedupe.

## 4. Decisions

- **One crate, five binaries**, matching the `testenv` crate's own precedent
  (`testenv`/`bd-meter`/`target-reap` from one `Cargo.toml`) rather than five crates for five
  scripts that already shared `cockpit/db.sh`.
- **`conf.sh` is self-sourced by every binary at startup** (`conf::self_source`, shelling out
  once to `bash -c '. conf.sh; env -0'` and applying the result to this process's own
  environment), exactly mirroring each bash original's own `. conf.sh` at its top. This was
  caught as a live bug during integration testing: an earlier draft of `layout` read
  `SPIRA_COCKPIT`/`COCKPIT_RIGHT_PCT`/etc. via bare `std::env::var`, assuming conf.sh's
  exports were already present — true for a developer's already-`. conf.sh`'d shell, false
  for `cockpit-ensure.service`, whose `ExecStart` sets only `SPIRA_RELEASE`+`PATH`.
- **`rebuild`/`layout`'s own bare-name calls to each other and to `health`** resolve through
  `$PATH` (`$SPIRA_RELEASE/bin`), never through `$SPIRA_COCKPIT` (where the *bash scripts*
  used to live, but the compiled binaries do not). Two call sites in the inherited draft
  still pointed at `$SPIRA_COCKPIT/rebuild` and `$SPIRA_COCKPIT/<role>` (for the
  restart-if-stale mtime check); both fixed to use the release's `bin/` during integration
  testing.
- **`COCKPIT_MAIL` has no Rust-side default.** An inherited draft defaulted an
  empty/unset `COCKPIT_MAIL` to `"aerc"` inside `layout`; the bash original's own
  `${COCKPIT_MAIL:-}` defaults to **empty** (no mail pane) and leaves any real default to
  `conf.sh`. A hardcoded fallback here would have silently overridden an operator's
  deliberate "no mail pane" the moment `conf.sh` agreed with bash and left it unset.
- **`descendants_of`/`descendant_count` never counts the root as its own descendant.** The
  bash `awk` this replaces (`rebuild.sh`'s `descendants_of`) starts its walk *at* each
  candidate pid and checks it against root before ever stepping to a parent — so a live
  root's own entry in its own process table matches on the first comparison, and the bash
  always over-counts a live server by exactly one. Ported as a deliberate fix (root's own
  entry is skipped outright), not a faithful reproduction of that quirk, because the
  consequence is real: `rebuild.sh --force`-gating would otherwise treat every live wedged
  server as holding at least one descendant, forever.
- **`reply`'s failure path now includes `bd`'s combined stdout+stderr**, matching what
  `resolve.sh` already did and `reply.sh` did not (the bash original's `reply.sh` discarded
  `bd`'s output entirely on failure, `>/dev/null 2>&1`). Named as an intentional difference,
  not silently ported: `resolve.sh`'s own comment already explains why discarding it once hid
  a database-wide write outage behind a generic "failed" line.
- **`resolve`/`reply`'s stdin path (`-`) is not trimmed of trailing newlines.** Bash's
  `$(cat)` strips every trailing newline from a captured reason/text; this port passes stdin
  through as `bd` receives it. Kept as a deliberate simplification rather than reproducing
  `$(cat)`'s exact stripping rule, since `bd close --reason`/`comments add` both accept and
  store arbitrary text either way — if this turns out to matter for a caller that pipes a
  file with a trailing newline, trimming can be reinstated in `main`, where stdin is read.
- **`sentinel_active` (health's halt banner) is a narrower, best-effort reimplementation**,
  not a port of `conf.sh`'s `spira_unit` helper: tries the per-instance unit name first,
  falls back to the plain name. `conf.sh`'s own rewrite is scheduled last in the bash
  inventory (group 4); porting even one of its helpers from inside this bead is exactly the
  scope creep "retire rather than port" exists to catch. If the instance-qualification rule
  changes, this one call site needs updating independent of `conf.sh`.
- **`layout::restart_spira_collector_if_stale` is intentionally empty.** The bash original
  checked this only because `health.sh` happened to source the same `conf.sh` as the
  collector; the collector's own liveness belongs to `spira/cockpit.sh`/`collect.sh`
  (sp-kt4l3, concurrent with this bead) and is not reimplemented here.
- **`tokens_section`'s sparkline is passed in as an already-rendered string**
  (`TokensExtra::tok_win_spark`), not drawn from raw history here. The bash original shells
  to `python3` per repaint to draw it from `$SPIRA_RUN/cockpit-history.csv`; carrying that
  history-reading and block-character scaling into this crate is a second piece of scope this
  bead did not need to take on to port the pane's layout and text. Currently always empty;
  wiring a real implementation is tracked as follow-up, not silently dropped.
- **`health`'s SIGINT/SIGTERM/SIGHUP handling uses a single raw `libc` `signal()` call**
  rather than pulling in a signal-handling crate, to restore the cursor and autowrap on an
  interactive Ctrl-C. `systemd`/a supervisor sending `SIGKILL` skips this, same as a bash
  `trap` racing a `SIGKILL` would.

## 5. Test strategy

- **Unit:** every formatting/classification/allocation function (`fit`, `tok`, `pct`,
  `model_short`, `dur_m`, `mail_dur`, `dot`/`num`/`bad_unless_zero`/`*_colour`, the snapshot
  parser and its bash-quote reversal, `share`'s three-tier allocator, `classify_argv`/
  `pane_role`, `classify_server_stderr`, `find_listening_inode`/`descendants_of`,
  `stale_clients`, `heal_ready`) has direct unit tests against both a normal and an
  absent/`?`/malformed input. 89 tests in `cargo test -p cockpit-ops --lib`.
- **Integration, by hand, against a fully isolated tmux server** (`TMUX_TMPDIR` pointed at a
  scratch directory — never the operator's socket, never `-L` against the default namespace
  where an un-isolated helper like `cockpit-remote` could still reach it): built a throwaway
  release root (`$SPIRA_RELEASE/bin/{health,layout,rebuild,resolve,reply}` = this bead's
  compiled binaries; everything else symlinked to the real release) and ran the full
  `rebuild` → `layout up/down/status/ensure` → crash-simulated `ensure` → `rebuild` cycle.
  10 of 11 of `rebuild`'s own positive-control verify checks passed against real tmux
  (session/window/pane creation, tagging, linking `brain:0`/`hunk:0` into `cockpit`, the
  health pane rendering real, non-blank content); the eleventh (composed session brief) is a
  real limitation of the sandbox, not of the port — it requires a real `claude
  --append-system-prompt` launch, which this test environment cannot perform.
  `resolve`/`reply` were run against a real, throwaway `bd` database (`bd init`): create,
  close, comment, stdin-mode, and the failure path (closing a nonexistent id) were all
  exercised and match the documented contract.
- **A safety near-miss worth recording:** an initial integration test invoked `rebuild`
  against the *default* tmux socket (the operator's real one) because `cockpit-remote` does
  not honor `TMUX_BIN`/`-L`; `TMUX_TMPDIR` (which redirects the default socket itself, so it
  isolates helpers that are not socket-aware) is the only reliable isolation for a test that
  calls out to `cockpit-remote`. No state on the operator's real cockpit changed (`build`/
  `sync` are idempotent against an already-correct state), but this crate's own tests below
  never repeat that mistake — every `Tmux` test seam is `TMUX_TMPDIR`-based.
- **Cannot be verified without production:** the composed-session-brief check (needs a real
  `claude` launch); the systemd-timer-invoked path of `layout ensure` (verified by code
  inspection of `cockpit-ensure.service`'s `Environment=` lines and by confirming `conf.rs`'s
  self-sourcing fixes the gap a bare-env-read version had, not by actually running under
  `systemd --user`).
