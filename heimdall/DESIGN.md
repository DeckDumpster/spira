# heimdall — the watchdog over the loop

Replaces `spira/heimdall.sh` (801 lines) and its own inline documentation, which this
document inherits almost verbatim because the constraints did not change by being
ported. Binary `heimdall`, invoked by bare name on the release `bin/` (sp-gypjk), exactly
where `heimdall.sh` was: the `ExecStart` of `spira-heimdall.service`, on a two-minute timer.

## 1. Intent

Heimdall's only power is speech. It reads timestamps and counters out of files, asks the
database one question, and compares what it found to thresholds. It does not repair,
restart, reclaim, land, kill or call anything, and it must not grow the ability to — a
watchdog that can act is a second controller with no supervisor of its own, and the first
thing it would do wrong is fight the sentinel over the same resource.

**Why it exists.** `law-file-it-and-let-the-loop-fix-it` says a harness defect is filed
and fixed by the loop, never by hand — including a fault that stops the loop. That is
only honest if something notices the loop has stopped and says so. Without this, the
statute's one hard case resolves to silence. It is a SEPARATE unit from the sentinel,
deliberately: a check cannot observe the failure of the thing running it.

**It must not be able to livelock,** and that constraint dictates everything below. No
git, no worktrees, no repo checkouts, no gate, no test run, no fetch. Every external call
carries a short timeout, and the systemd unit carries `TimeoutStartSec=60`: a pass that
cannot finish is killed, and a killed pass stops writing the heartbeat — the one symptom
that must never be silent.

**Its own death is visible, by construction.** The heartbeat (`heartbeat.rs`) is written
on EVERY run including a failing one, and the ops pane renders its age and marks it stale
rather than omitting it — a silent escalator and a healthy system must not look identical
(`law-absence-needs-a-positive-control`).

**Where the alert goes, and in which order.** The control plane is the record: one
condition per CAUSE at `conditions/heimdall/<key>` (`spira-lc condition set|clear|list`), type
`Alert`, never a bd item. The fallback file (`fallback.rs`) is written if and only if the
store could not be read or written — its EXISTENCE is itself the statement "the store could not
be reached" — and removed the moment the store answers again, so there is never a second
source of truth standing beside a working first one.

**The condition is a contract with the attention panel** (`cockpit/panel/src/store.rs`): the
message's first line is the title, the store counts `flaps`, and `acked` and `silent_until`
are the panel's to write. The store forgets `acked` when the condition RETURNS; Heimdall never
touches `silent_until`.

**Noise is the failure mode.** Three defences, in this order of how much they matter: (1)
thresholds measured from the real pass log (`judge.rs`, a port of the former
classifier script — see §2.4); (2) a condition must hold for `SPIRA_HEIMDALL_CONFIRM` consecutive passes before it
fires and clear for `SPIRA_HEIMDALL_CLEAR` before it is retracted; (3) one condition per
cause forever, re-raised and flap-counted.

## 2. Contract

### 2.1 Invocation

`heimdall [--home DIR] [--report]` — `--report` classifies and prints, writing nothing.
`--home` defaults to `$SPIRA_HOME`, else `$SPIRA_RELEASE/spira` (`main.rs::resolve_home`);
the unit passes `--home @SPIRA_PROD@` explicitly, the same convention `dwarf`'s launchers
use.

### 2.2 Exit codes

`0` on every ordinary pass, including one that found nothing firing, one that raised or
cleared an alert, and `--report` with nothing firing. `1` only when `--home`/conf.sh
cannot be resolved at all (a setup failure, not a condition to report).

### 2.3 Architecture — gather, classify, reconcile, speak

```
gather -> judge::classify -> speak
```

unchanged in shape. Each arrow-side is a module:

| module | heimdall.sh section | pure? |
|---|---|---|
| `gather.rs` | GATHER | no — file reads, systemctl |
| `restart.rs` | the restart-baseline arithmetic inside GATHER | **yes** |
| `classify.rs` | the JSON built for the classifier, and the `heimdall classify` stdin entry | **yes** |
| `judge.rs` | the classifier: observations in, firing records out | **yes** |
| `reconcile.rs` | the confirm/clear/refresh loop | **yes** — the whole debounce state machine, no IO |
| `alerts.rs` | `body_of`, `alert_write`, `alert_clear`, the write probe | no — store calls, but against a `Conditions` port so it is unit-tested with a fake |
| `conditions.rs` | `spira-lc condition` calls threaded through the reconcile loop | no — the port |
| `fallback.rs` | the fallback-file write/remove | no — but its JSON shape is a pure function, tested |
| `heartbeat.rs` | the heartbeat write | no — same split |
| `state.rs` | the TSV state file | **yes** (parse/render) |

### 2.4 The classifier: `judge.rs`

The whole judgement is a pure function of one observation object (`judge::classify`): no
clock, file, command or network. `heimdall classify` reads one observation on stdin and writes
one sorted-key JSON alert per line, which is how `test-heimdall-classify.sh` replays it
against real sentinel log. `classify::run` calls it in-process.

### 2.5 The store port (`conditions.rs`)

`spira-lc condition list|set|clear --owner heimdall`, under `timeout` (`SPIRA_HEIMDALL_BD_TIMEOUT`,
10 s): a store that has not answered in ten seconds is unreachable as far as a watchdog is
concerned, and a timeout is reported as saturation, not down.

### 2.6 Files read and written

| path | mode | what |
|---|---|---|
| `$SPIRA_RUN/heimdall.state` | r/w | the TSV debounce state (`state.rs`) |
| `$SPIRA_RUN/heimdall.status` | w | the heartbeat, every run |
| `$SPIRA_RUN/heimdall.alerts.json` (or `SPIRA_HEIMDALL_FALLBACK`) | w/rm | the fallback channel |
| `$SPIRA_RUN/sentinel.log` (or `SPIRA_HEIMDALL_SENTINEL_LOG`) | r | tailed, bounded |
| `$SPIRA_RUN/strands.json` (or `SPIRA_HEIMDALL_STRANDS`) | r | strand.sh's episode JSON, passed through opaque |
| `$SPIRA_REPO/raw/spira-beads/spira.jsonl` (or `SPIRA_HEIMDALL_MIRROR`) | r (mtime only) | the beads mirror staleness check |
| `$SPIRA_RUN/world.halted`, `world.draining` | r | the halt/drain gate readings |

### 2.7 Control-plane conditions

`condition list` (the read probe), `condition set <key> --type Alert --status True` (raise and
re-raise), `condition clear <key>`, and `condition set heimdall-write-probe --type WriteProbe
--status False` on every pass (the write probe: one key, never firing, nothing to duplicate).

## 3. Decisions

1. **`heimdall` is its own crate, not a module of `sentinel` or `dwarf`.** No existing crate
   owned this area (heimdall watches the LOOP, including the sentinel, so living inside
   sentinel would be the watchdog housed inside what it watches) — new crate, per the
   wave brief's own rule ("moves into the crate that already owns the area, or a new
   crate only if none fits").
2. **Alerts are control-plane conditions, not bd items.** An alert's state is a fact the
   control plane owns; keeping it in bd made alerts most of bd's rows. The lib.sh seam that
   carried `bdq` and `bead_reopen` went with the bd writes.
3. **The write probe is one condition written False on every pass**, so there is no probe
   bead to re-derive, adopt or duplicate.
4. **`spira_unit`, `log`, `die`, `json_only` are reimplemented directly in Rust, not
   shelled out.** They carry no graph semantics to drift from — `spira_unit` is twelve lines of systemctl calls (`gather::spira_unit`),
   `log`/`die` are one `printf` (`util::log_line`), `json_only` is a `sed` one-liner
   (`util::json_only`, unit-tested against the exact warning-banner case bd produces).
5. **`iso()` (heimdall.sh's timestamp formatter, `SPIRA_TZ`-aware) still shells to `date`**
   (`util::iso_display`) rather than being reimplemented with a timezone table: only the
   system's own tzdata resolves an IANA zone faithfully, and this value only ever reaches
   a bead body a human reads — low stakes, no reason to carry a timezone-database
   dependency for it.
6. **The restart-baseline arithmetic (`restart.rs`) is factored out as its own pure
   module**, not inlined in `gather.rs`, because it is the one piece of GATHER with real
   decision logic (baseline reset on a count going down, window-expiry slide) worth
   holding to its own unit tests independent of any systemctl fake.
7. **No `--sweep`-style mode, no rune.** Heimdall is not a persona and claims nothing; its
   CLI is `[--home DIR] [--report]` only, unlike `dwarf`'s richer grammar.

## 4. Parity evidence

See the delivery report (bead sp-zpaq0) for the `testenv` run of `test-heimdall.sh`
(repointed from `bash "$SH/heimdall.sh"` to `command heimdall --home "$SH"`, its fixture
otherwise unchanged) against this binary, and the wall-clock comparison of one pass
old vs new.

## 5. Tests

`cargo test -p heimdall` — 95+ unit tests: `state.rs` (TSV round-trip, the older short-line
spelling), `reconcile.rs` (every debounce transition: one sighting does not fire, a
confirmed raise, a flap does not duplicate, one clear sighting does not retract, a
confirmed clear, the refresh interval and its hand-closed-bead skip, db-unreachable holds
counters but never triggers), `restart.rs` (the baseline arithmetic: first-run floor, a
count going down resets, a window expiring slides, a standing burst does not re-alert),
`alerts.rs` (a raise then a clear moves the condition True then False and writes no bd item,
a clear with nothing firing writes nothing, the probe never fires and a timeout is saturation,
against a `Conditions` fake), `gather.rs` (log-tail truncation, mirror
configured/missing/present, world halt/drain parsing, `spira_unit`'s fallback chain),
`fallback.rs`/`heartbeat.rs` (JSON/env-file shape, atomic write leaves no `.tmp`), `main.rs`
(`resolve_home`'s three-way fallback).
