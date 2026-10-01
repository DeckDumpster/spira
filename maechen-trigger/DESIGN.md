# maechen-trigger

Rust port of `spira/maechen-trigger.sh` (sp-0ekp7, rewrite wave 7b, law-new-subsystems-are-rust,
law-rust-rewrites-start-from-intent). `spira/maechen-trigger.sh` becomes a thin shim —
`. conf.sh; exec maechen-trigger "$@"` — the same pattern `gate.sh`, `czar.sh` and
`reconciler.sh` already use for their own Rust replacements.

## Intent

Evaluate, on a timer (`spira-maechen.timer`, 10 min), whether the Maechen persona should be
summoned to run a retrospective pass over recent failures. File **at most one** open trigger
bead at a time; never let a live trigger accumulate a backlog of duplicates.

Three independent conditions, any one of which files (or — since they share one dedup guard —
would-file) the same single bead:

1. **Landing trigger.** Enough commits naming a bead id have landed on every managed
   repository's base branch since the watermark (`SPIRA_MAECHEN_LANDING_INTERVAL`, default 25).
2. **Time trigger.** Too long has elapsed since the last completed Maechen pass
   (`SPIRA_MAECHEN_MAX_GAP_SECONDS`, default 10800s), independent of landing volume — a
   quiet system still gets periodically reviewed.
3. **Invalid-closed trigger.** `detect_invalid_closed` (the `lib.sh` seam) found at least one
   `INVALID-CLOSED` or `UNFILED-FOLLOW` row the operator has not already allowlisted.

## Contract

```
maechen-trigger
```

No arguments, no subcommands (matches the bash: the script takes none). Reads its
configuration from environment (`SPIRA_RUN`, `SPIRA_HOME`, `SPIRA_DB`, `SPIRA_BD`,
`SPIRA_REPO_MAP`, `SPIRA_SCOPE_LABEL`, `SPIRA_MAECHEN_LABEL`, `SPIRA_MAECHEN_LANDING_INTERVAL`,
`SPIRA_MAECHEN_MAX_GAP_SECONDS`, `SPIRA_MAECHEN_MAX_BEADS`), exactly as the bash did, so
`conf.sh` and the systemd unit need no changes.

Exit code:
- `0` — a bead was filed, dedup already found one open/in-progress, the lane is unadmitted,
  or no trigger condition fired. All four are "correct, nothing more to do."
- `1` — `bd create` failed after a trigger fired. The watermark and lastpass files are
  **never written by this binary** — `lib.sh`'s Maechen pass step 5 owns both, same as today.

Side effects (unchanged from the bash):
- A non-blocking `flock` on `$SPIRA_RUN/maechen-trigger.lock`; a caller that loses the race
  skips this tick silently (exit 0) rather than risking two overlapping list-then-create
  dedup checks.
- One `bd -C $SPIRA_DB create` on a fire, **not** through `bdq` — this preserves an existing
  asymmetry with `incident.sh` (which always uses `bdq`) rather than introducing a behaviour
  change (see Decisions).
- Log lines to stderr, `<ISO-8601Z> maechen-trigger: <message>`.

## Schema / architecture

- `engine.rs` — pure decision logic, no IO: commit-subject → bead-id extraction (the three
  landing forms plus the aeon-prefix form), repo-map line parsing, the two numeric trigger
  predicates, invalid-closed row filtering/dedup, and all of the bead title/description/label
  string assembly. Every function is a table-driven unit test; this is the surface `sp-0ekp7`'s
  parity proof rests on.
- `ports.rs` — the `World` trait: the seam between decision logic and everything stateful.
- `real.rs` — `World` for production: shells to the **`lib.sh` seam** (source `lib.sh`, call
  one function, read stdout — the identical pattern `gate-check/src/real.rs` already
  established) for `spira_home_repo`, `repo_root`, `spira_landref`, `spira_lane_admitted`,
  `spira_open_trigger_count` and `detect_invalid_closed`; shells to `bd` directly for
  `create`; shells to `git log` for landing counts; reads the watermark/lastpass files and
  the repo-map file directly off disk.
- `main.rs` — orchestration matching the bash's control flow 1:1: lock → dedup → lane check →
  read clocks → count landings (home repo, then repo-map, skipping the home repo) → evaluate
  the three triggers → file or exit.

## Non-goals (out of scope for this bead)

- **`spira_lane_admitted`'s fayth-label scan, `spira_open_trigger_count`, `detect_invalid_closed`.**
  These are `lib.sh`'s own functions (families V and T), shared with `groom-trigger.sh`, the
  census and the closed-record review. `lib.sh` is explicitly the *last* component in the wave
  order (wave-brief "Order is the inventory's ... lib.sh last"); reimplementing any of them
  here now would create a second, driftable copy of logic lib.sh's own wave will port
  properly. They stay a subprocess seam.
- **`repo_field`, `spira_landref`'s resolution ladder — no longer true.** Family U (the repo
  registry) and family W (base refs) ported to `spira_config::repos` in sp-37rmg/sp-o88bx
  ("wave 4.11"/"4.12"); `real.rs`'s `home_repo`/`repo_root`/`landref` call that in-process now
  (sp-k6lku, "wave 4.13") instead of a `bash -c '. lib.sh; ...'` seam that itself only shelled
  into the `spira-config` binary a second time. This is calling the canonical port, not a
  second copy of it.
- **`mail.sh`.** Not used by `maechen-trigger.sh` at all (only `incident.sh` sends mail); no
  change needed here.
- **`groom-trigger.sh`.** Shares `spira_open_trigger_count`/`spira_lane_admitted` with this
  script (duplicate cluster D14) but is a separate bead's scope (not in wave 7b's inventory).

## Parity

- `engine.rs`'s 21 unit tests include a fixture window of real commit subjects captured
  verbatim from this repository's own `git log --format=%s` history, so the landing-id
  extraction is checked against real data, not only hand-written examples.
- Black-box parity: `test-maechen-trigger.sh` (T2, stub `bd`, real `git` against throwaway
  repos) exercises the CLI contract — trigger conditions, dedup, labels, exit codes — against
  whichever `maechen-trigger` is first on `PATH`. Run unchanged against the compiled binary
  through its existing shim, it is this bead's parity evidence: same suite, same fixtures,
  same assertions, old implementation vs new.

## Decisions

- **The bash calls `bd create` directly, not `bdq`** — Rust preserves that literally rather
  than "fixing" it by routing through `bdq`'s extra fences (repo-label validation, the
  destructive-phrase and schema-delete refusals). Changing that posture is not this bead's
  call; noted here so a future reader does not mistake it for an oversight.
- **Invalid-closed dedup is by an exact bead-id set, not the bash's substring-containment
  check** (`case "$acc" in *"$_bid"*)`). The two agree on every realistic input — a bead id is
  never a substring of another row's text — and an exact set is simpler to reason about and
  test. Named here as the one deliberate behavioural difference (wave-brief "name any
  intended difference").
- **No `[[bin]]` split for `groom-trigger.sh`.** Despite sharing two `lib.sh` helpers, it is
  out of this bead's named scope (`spira/incident.sh spira/maechen-trigger.sh`) and is left
  as bash.
