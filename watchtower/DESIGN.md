# watchtower — hand Ops the state of the pipeline

Replaces `spira/watchtower.sh` (1,596 lines) and `spira/watchtower-czar-outcome.py` (69
lines, inlined — its only caller was watchtower.sh's `--czar-outcome-check`). The binary is
`watchtower` and takes the same five entry points minus one retired stub (§4).

## 1. Intent

Ops has always been able to hear about a unit that CRASHED — incident.sh files from a
systemd `OnFailure`, and that is its only intake. A queue that has stopped moving while
every unit is happily `active` is invisible to it: on 2026-09-07 nothing landed for the
better part of a morning while every process was green, every log line was individually
true, and eleven consecutive gate runs correctly reported that the next pass would take it.
Nothing noticed. The operator did, by looking.

So this is the second intake: not "a process died" but "the pipeline is not doing the thing
it exists to do." The system IS, stated plainly, N workers pulling from a DAG into a merge
queue — so the numbers that matter are the depth of that queue, how long its oldest member
has waited, and how long it has been since anything came out of the far end.

**It does not decide** (the operator's call, 2026-09-07, over a threshold-driven detector).
This program gathers and hands over; an Ops aeon reads the snapshot and decides what is
wrong and what beads to cut. Thresholds anticipate only the outage already had — every
stall so far has been a shape nobody had a number for.

**It is deterministic and cheap, deliberately** (law-deterministic-before-inference). It
reads files that already exist and shells out to nothing slow, because the one thing a
watchtower may not be is another thing that is down during an outage.

**A field it could not read renders `?`, never 0** (law-absence-needs-a-positive-control).
A broken probe reporting "0 branches waiting" is an all-clear that displaces the suspicion
which would have prompted a look — the exact failure this whole program is a response to.
Every collector in this crate upholds that: an unreadable input is `Field::Unknown`, never a
silent zero.

## 2. Contract — the five entry points

```
watchtower                    gather, and file the sweep Ops claims (or SWEEP:NOMINAL)
watchtower --show             gather and print; touch nothing
watchtower --throttle-check       admission gate for the task pool (sentinel CHECK 6)
watchtower --czar-outcome-check   czar-trigger unclaimed / outcome-not-cleared (CHECK 6)
watchtower --pr-stall-check       PR-mode stall detector: red / auto-merge-off / conflict / arm (CHECK 6)
watchtower --lock-holders-check  on a queue stall (certified waiting, no landing for SPIRA_QUEUE_THROTTLE_STALL_MINS), names every pid holding a landing/gate lock file by /proc/*/fd and flags non-gate holders as leaked-lock suspects
```

All five are read-mostly: the sweep and the four checks write at most a stamp file, a
state file and (via `incident.sh`) a bead — the same side effects as the bash, on the same
paths, so nothing downstream (sentinel's stamp read, the cockpit pane, incident.sh's own
dedup) needs to change.

Every check refuses to escalate on a `?`/unknown input (fail closed on the *escalation*,
never on the *read* — a probe that could not read still exits 0 and logs why, matching the
bash's `[ -r "$X" ] || { log …; exit 0; }` guards).

## 3. What moved, and what stayed bash

`incident.sh`, `ctrl.sh`, `world.sh`, `yield.sh`, `testenv`, `branch-guard.sh`,
`release`, `cockpit/moot-sweep.sh` are **not in this bead's scope** (other rewrite waves, or
not yet scheduled) and stay bash/binary dependencies called exactly as the bash called them:
by bare name (or `SPIRA_*_SH` override) on the release PATH, same argv, same env, same
stdin/stdout contract. Repointing those is each script's own wave.

`world.sh`'s `TIMER_PRIORITY` array and `ctrl.sh`'s suspension map (`ctrl_load_suspended`)
are bash-library state with no CLI form; `--disabled-timer-check` gets them through a fixed
`bash -c` seam (`seams::TIMER_PRIORITY_AND_SUSPENDED`, §6) — the same pattern
`sentinel/src/seams.rs` uses for lib.sh, not a second hand-written timer list here (that
duplication is exactly what let a timer added after a hand list was written escape a stop).

## 4. Decisions — what was dropped

- **`--queue-checks` is deleted, not ported.** It has been a one-line retirement stub since
  sp-rpibz moved the six queue-stall detectors into `czar-pass --pass` (czar.sh, the shim
  that once ran it, retired by sp-8fsql); `test-czar-pass.sh`
  already asserts nothing calls it. grep across systemd units, the sentinel crate, and every
  `.sh`/`.rs` caller in the release tree turned up zero live callers — only the stub itself
  and comments. Dropped entirely; `watchtower --queue-checks` is no longer a valid argument.
- **`watchtower-czar-outcome.py` is inlined, not repointed.** Its only caller was
  `--czar-outcome-check`; with that ported to Rust the classifier moves in-process
  (`czar_outcome::classify`) instead of a second process fork per sentinel pass. The script
  and both suites that tested it (`test-watchtower-czar-outcome-classify.sh`,
  `test-watchtower-czar-outcome.sh`) retire; `czar_outcome::tests` covers the classifier
  one-for-one against the same fixtures, and `czar_outcome::gather` gets its own test against
  a fake `bd`.
- **Nothing else in the sweep's field list was dropped.** Retiring a vital sign requires
  proving its producer (`collect.sh`/`cockpit.sh`, a different wave) is also gone; that
  evidence was not available in this bead's scope, so every `SP_*` key the bash read is read
  here unchanged.

## 5. Schema — what each check reads and writes

### `--throttle-check`
- Reads: `$SPIRA_RUN/world.halted`; landstate records under `$SPIRA_RUN/landstate/`
  (`CERTIFIED`/`LANDED` rows); a git ref check per live `CERTIFIED` id
  (`refs/heads/spira/<id>` exists, tip not yet an ancestor of the land ref); `git log` for
  the async-gate deliberate-stall carve-out (`sp-c8w16`/`sp-74gwk` on the land ref).
- Writes: `$SPIRA_THROTTLE_STAMP` (default `$SPIRA_RUN/queue-throttled`) on engage, removed
  on lift or override-off; an `incident.sh` bead on engage, lift, or stall-fault.
- Env seams (unchanged names): `SPIRA_THROTTLE_STAMP`, `SPIRA_QUEUE_THROTTLE_OVERRIDE`,
  `SPIRA_INCIDENT_SH`, `SPIRA_QUEUE_THROTTLE_DEPTH_AT` (16), `SPIRA_QUEUE_THROTTLE_RELEASE_AT`
  (8), `SPIRA_QUEUE_THROTTLE_STALL_MINS` (50), `SPIRA_TC_REPO`, `SPIRA_TC_LAND_REF`,
  `SPIRA_REPO`, `SPIRA_DB`, `SPIRA_HOME_REPO`.

### `--czar-outcome-check`
- Reads: `bd -C $SPIRA_DB list --label czar-trigger --all --json --limit 0 --brief`.
- Writes: one `incident.sh` bead per `UNCLAIMED`/`NOT_CLEARED` finding, ref
  `incident:czar-unclaimed-<id>` / `incident:czar-not-cleared-<id>`.
- Env seams: `SPIRA_CZAR_OUTCOME_MINS` (30), `SPIRA_CZAR_UNCLAIMED_MINS` (10),
  `SPIRA_CZAR_LABEL` (czar-trigger), `SPIRA_INCIDENT_SH`, `SPIRA_DB`.

### `--pr-stall-check`
- Reads: landstate records (`REBASED` rows with `pr-open:<repo>` reason); `gh pr view
  --json mergeable,statusCheckRollup`; `gh repo view --json allowAutoMerge`.
- Writes: an `incident.sh` bead (red-check / auto-merge-off), `rm` the landstate record
  (CONFLICTING), or `gh pr merge --auto --squash` (otherwise).
- Env seams: `SPIRA_PR_STALL_MINS` (60), `SPIRA_GH` (gh), `GH_TIMEOUT` (120),
  `SPIRA_INCIDENT_SH`, `SPIRA_DB` — plus whatever `seams::repo_root` needs to resolve the
  repo (`SPIRA_REPO_MAP`, etc.); it sources lib.sh from `main::lib_sh_dir` (§6), never from
  `$SPIRA_HOME`.

### `--disabled-timer-check`
- Reads: `world.sh`'s `TIMER_PRIORITY` and `ctrl.sh`'s suspension map (via the seam, §3);
  `systemctl --user is-enabled`/`is-active` per timer (with and without the
  `SPIRA_INSTANCE` suffix, matching the bash's fallback).
- Writes: an `incident.sh` bead per disabled-and-unsuspended timer, ref
  `incident:disabled-timer-<base>`.
- Env seams: `SPIRA_SYSTEMCTL` (systemctl), `SPIRA_INSTANCE`, `SPIRA_INCIDENT_SH`, `SPIRA_DB`,
  `SPIRA_HOME`.

### `--conditions-check`
- Four probes (`probes.rs`) each report what stands now; `conditions.rs` turns that into one
  incident bead per condition, closes it when the condition clears, and does nothing for a probe
  that could not read. Standing across passes files no second bead (an open bead for the ref
  holds it); a `sustain` window counts from the latest crossing.
- Probes: a `SPIRA_RELEASE_CURRENCY_UNITS` unit rendering a pinned release other than `current`, or rendering `current` while its MainPID runs from another release, for
  `SPIRA_RELEASE_STALE_SECS`; a spira unit failed `SPIRA_FAILED_UNIT_RUNS` consecutive
  invocations (shares `incident:failed-unit-<unit>` with the sweep, so the two never double-file);
  `/tmp` free under `SPIRA_TMPFS_SHED_FREE_MIB`, root free under `SPIRA_DISK_FLOOR_PCT`, io/memory
  PSI `full avg60` at `SPIRA_PSI_FULL_AVG60` for `SPIRA_PSI_SUSTAIN_SECS`; release trees over
  `SPIRA_RELEASES_KEEP` + `SPIRA_RELEASE_STORE_SLACK`.
- Writes: `$SPIRA_RUN/conditions/{filed,pending}/<probe>/<key>`, `failed-runs.tally`.

### Sweep (default / `--show`)
Unchanged field list and heredoc layout from the bash (see `render::render` doc comment for
the full field-by-field source map). Writes `$SPIRA_WATCH_PROMPT_FILE` (default
`$SPIRA_RUN/ops-sweep-prompt.txt`) atomically, advances `$SPIRA_RUN/lapsed.swept` only on a
successful write, and (default mode only, non-halted, non-nominal) files the same nine
threshold escalations the bash filed: drain, sending-oldest-unsent, unadopted-refs, hotfix,
batched-stranded, batched-too-long, closed-stranded, dedup-meter, idle-while-ready — then
runs `cockpit/moot-sweep.sh --apply`.

## 6. Seams

- **`main::lib_sh_dir` vs `main::spira_home` — two different questions, not one.**
  `lib_sh_dir` is where lib.sh's CODE lives, found via `incident::which("lib.sh")` on
  `$PATH`. The bash always had lib.sh's functions by sourcing it relative to `$0`
  (`. "$(dirname "$0")/lib.sh"`) at the top of watchtower.sh, regardless of what
  `$SPIRA_HOME` was set to — a compiled binary has no `$0` directory to be relative to, so
  this is the equivalent self-location. Every seam that needs lib.sh's code uses it:
  `git::spira_landref`, `seams::repo_root`, `seams::pipeline_probe`. `spira_home` is
  `$SPIRA_HOME` the environment variable (falling back to `lib_sh_dir` only when unset),
  used only by `seams::timer_priority_and_suspended` — the bash reached world.sh/ctrl.sh
  via a literal `"${SPIRA_HOME}/world.sh"`, genuinely `SPIRA_HOME`-driven, not `$0`-relative.
  Conflating the two was a real bug (sp-lnmbq): a caller that points `SPIRA_HOME` at a
  fixture directory holding data files but no lib.sh (test-watchtower.sh's idle-while-ready
  section, which overrides `SPIRA_HOME` so `bulk_ready_by_fayth`'s chamber lookup reads its
  fixture, while still expecting the real lib.sh code to run) made every lib.sh-sourcing
  seam fail closed with no error. `SPIRA_HOME` still reaches lib.sh's own functions
  correctly either way — it is inherited in the seam's subprocess environment, never
  passed as the sourcing path.
- `incident::file` — spawns `bash $SPIRA_INCIDENT_SH file <title> -` with the same
  `SPIRA_INCIDENT_*` env and body-on-stdin contract as every bash call site.
- `seams::timer_priority_and_suspended` — one `bash -c` that sources `world.sh`
  (`WORLD_LIB=1`) and `ctrl.sh` (`CTRL_LIB=1`) and prints `TIMER_PRIORITY` plus the
  suspended-base list, NUL-separated. Fixed script, no interpolation — the base names come
  back on stdout, never assembled into the script text.
- `czar_outcome::classify` is a pure function over `&[TriggerBead]` — no IO, table-tested
  against the exact fixtures `test-watchtower-czar-outcome-classify.sh` used.
- `throttle::decide`, `pr_stall::decide` are pure decision functions over already-gathered
  inputs, same split the bash used when it hoisted `collect_*` above the main guard for
  direct testability.

## 7. Parity proof

See the delivery report for the old-vs-new comparison run against the same fixtures the
retired suites used (throttle critical pair + hysteresis + stale-CERTIFIED filter,
czar-outcome classify permutations, pr-stall's seven branches, disabled-timer against a
fake systemctl).

## 8. What is deliberately NOT here

- **Not a second timer list.** §3.
- **Not the collector.** `collect.sh`/`cockpit.sh` write `cockpit.env`; this only reads it.
- **Not the yield meter.** `yield.sh` computes `YIELD_*`; this shells to it exactly as the
  bash did (out of scope this bead).
- **Not incident.sh's dedup.** That logic (lookback window, `SPIRA_SIN_EXEMPT`) lives in
  incident.sh; this crate only calls it with the same `SPIRA_INCIDENT_*` env the bash used.
