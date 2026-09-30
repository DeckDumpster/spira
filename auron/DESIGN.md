# auron — the watchdog over the loop

Replaces `spira/auron.sh` (801 lines) and its own inline documentation, which this
document inherits almost verbatim because the constraints did not change by being
ported. Binary `auron`, invoked by bare name on the release `bin/` (sp-gypjk), exactly
where `auron.sh` was: the `ExecStart` of `spira-auron.service`, on a two-minute timer.

## 1. Intent

Auron's only power is speech. It reads timestamps and counters out of files, asks the
database one question, and compares what it found to thresholds. It does not repair,
restart, reclaim, land, kill or summon anything, and it must not grow the ability to — a
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

**Where the alert goes, and in which order.** Beads is the record: one bead per CAUSE,
type `event`, labelled `alert`, `overseer` and `alert:<key>`. The fallback file
(`fallback.rs`) is written if and only if the beads path failed — its EXISTENCE is itself
the statement "the database could not be reached" — and removed the moment beads answers
again, so there is never a second source of truth standing beside a working first one.

**Not `needs-ryan`.** That label means a decision only the operator can make and is
excluded from every fayth's predicate; an alert is a CONDITION, not a decision.

**The labels are a contract with the attention panel** (`cockpit/panel/src/store.rs`),
not decoration: `alert` AND `overseer`, `flaps:<n>`, and two labels this program must
respect rather than write — `acked` (the operator has seen it; Auron removes it only when
the condition RETURNS) and `silent-until:` (the panel's own affordance; Auron never
touches it).

**Noise is the failure mode.** Three defences, in this order of how much they matter: (1)
thresholds measured from the real pass log (`auron-classify.py`, unchanged by this port —
see §4); (2) a condition must hold for `SPIRA_AURON_CONFIRM` consecutive passes before it
fires and clear for `SPIRA_AURON_CLEAR` before it is retracted; (3) one bead per cause
forever, reopened and flap-counted.

## 2. Contract

### 2.1 Invocation

`auron [--home DIR] [--report]` — `--report` classifies and prints, writing nothing.
`--home` defaults to `$SPIRA_HOME`, else `$SPIRA_RELEASE/spira` (`main.rs::resolve_home`);
the unit passes `--home @SPIRA_PROD@` explicitly, the same convention `aeon`'s launchers
use.

### 2.2 Exit codes

`0` on every ordinary pass, including one that found nothing firing, one that raised or
cleared an alert, and `--report` with nothing firing. `1` only when `--home`/conf.sh
cannot be resolved at all (a setup failure, not a condition to report).

### 2.3 Architecture — gather, classify, reconcile, speak

```
auron.sh gathers -> auron-classify.py classifies -> auron.sh speaks
```

unchanged in shape. Each arrow-side is a module:

| module | auron.sh section | pure? |
|---|---|---|
| `gather.rs` | GATHER | no — file reads, systemctl, journalctl |
| `restart.rs` | the restart-baseline arithmetic inside GATHER | **yes** |
| `classify.rs` | the JSON built for the classifier, and parsing its answer | mostly (the JSON build is pure; the subprocess call is not) |
| `reconcile.rs` | the confirm/clear/refresh loop | **yes** — the whole debounce state machine, no IO |
| `alerts.rs` | `body_of`, `alert_write`, `alert_clear`, the write probe | no — bd calls, but against a `BdOps` port so it is unit-tested with a fake |
| `bdops.rs` | `bdq` calls threaded through the reconcile loop | no — the seam |
| `fallback.rs` | the fallback-file write/remove | no — but its JSON shape is a pure function, tested |
| `heartbeat.rs` | the heartbeat write | no — same split |
| `state.rs` | the TSV state file | **yes** (parse/render) |
| `seam.rs` | (new) | no — the lib.sh subprocess seam |

### 2.4 What is NOT ported: `auron-classify.py`

Out of this bead's scope (sp-zpaq0 names `spira/auron.sh spira/escape.sh
spira/work-env.sh` only; `auron-classify.py`/`escape-classify.sh` are the next inventory
row). It is unchanged, has a live caller (this binary, exactly as it had auron.sh), and
keeps its own suite (`test-auron-classify.sh`). `classify::run` calls it by bare name on
stdin/stdout, byte for byte what auron.sh's heredoc-and-pipe did.

### 2.5 The lib.sh seam (`seam.rs`)

Three functions, each `bash -c <FIXED>` with every datum NUL-framed on stdin — the same
design as `aeon/src/seam.rs`, independently instantiated (no cross-crate dependency; see
§6 Decisions):

- `_auron_bdq` — `bdq "$@"`. Kept as a seam call rather than reimplemented because `bdq`
  carries real guards (repo-label refusal, destructive-vocabulary refusal, the schema-
  delete refusal, the connection retry, `SPIRA_BDJSON_FIXTURE`) that this program's own
  creates and updates must still honour, and duplicating them in Rust is a second copy to
  drift from lib.sh's.
- `_auron_bead_reopen` — `bead_reopen "$@"`. Carries landstate-withdrawal and
  submitted-label interaction that a bare `bd reopen` does not have.
- `_auron_snapshot` — `env -0`, conf.sh's resolution dumped whole (SPIRA_RUN, SPIRA_DB,
  SPIRA_REPO, SPIRA_EXPORTER, SPIRA_INSTANCE, ...). auron.sh got these by sourcing lib.sh
  itself once at the top of its own process; this binary cannot source bash, so it asks
  for the same resolution through one seam call at startup.

`BD_TIMEOUT` is set explicitly in the seam subprocess's own environment (`main.rs`) rather
than relied upon being "in scope": a `bash -c` seam call is a fresh process, not a
subshell of this one, so a bash variable set here would not reach it.

### 2.6 Files read and written

| path | mode | what |
|---|---|---|
| `$SPIRA_RUN/auron.state` | r/w | the TSV debounce state (`state.rs`) |
| `$SPIRA_RUN/auron.status` | w | the heartbeat, every run |
| `$SPIRA_RUN/auron.alerts.json` (or `SPIRA_AURON_FALLBACK`) | w/rm | the fallback channel |
| `$SPIRA_RUN/sentinel.log` (or `SPIRA_AURON_SENTINEL_LOG`) | r | tailed, bounded |
| `$SPIRA_RUN/strands.json` (or `SPIRA_AURON_STRANDS`) | r | strand.sh's episode JSON, passed through opaque |
| `$SPIRA_REPO/raw/spira-beads/spira.jsonl` (or `SPIRA_AURON_MIRROR`) | r (mtime only) | the beads mirror staleness check |
| `$SPIRA_RUN/world.halted`, `world.draining` | r | the halt/drain gate readings |

### 2.7 Bead-store rows

`list --all --limit 0 --label alert --json` (the read probe), `list --all --limit 0
--label auron:probe --json` (probe re-derive), `create --type event -p 1|-p 0`, `update`,
`close --reason`, `label add|remove`, `reopen` (through `bead_reopen`) — every one through
`bdq`/`bead_reopen`'s seam, never a bare `bd`.

## 3. Decisions

1. **`auron` is its own crate, not a module of `sentinel` or `aeon`.** No existing crate
   owned this area (auron watches the LOOP, including the sentinel, so living inside
   sentinel would be the watchdog housed inside what it watches) — new crate, per the
   wave brief's own rule ("moves into the crate that already owns the area, or a new
   crate only if none fits").
2. **The lib.sh seam is duplicated, not shared with `aeon`'s.** Both are `bash -c <FIXED
   NUL-framed script>`, and the shapes rhyme on purpose, but auron has no fayth, no
   `FAYTH_REQUIRE_LABEL` folding, and a three-function allowlist instead of aeon's
   thirty-odd — sharing would mean auron depending on aeon's crate for an unrelated
   concern, against this workspace's own convention of small independent crates (each
   port carries its own `util.rs`; `sentinel/src/host.rs` and `aeon/src/util.rs` are not
   shared either).
3. **`maintain_probe`'s "create after a failed update is a recovery" property
   (`_probe_had` in the bash) is ported as it actually behaves, not as its comment
   describes.** Traced through: bash's own guard (`if [ -z "$PROBE_ID" ]`) means CREATE is
   only ever reached on a pass where the cached id was already empty at the top — which is
   exactly when `_probe_had` is too, so `[ -n "$_probe_had" ]` inside the create branch is
   always false. A create always reports `db_write_ok=1` on success; see `alerts.rs`'s own
   doc comment on `maintain_probe`. Named here rather than silently dropped
   (law-rust-rewrites-start-from-intent: name each dropped/changed behaviour).
4. **`spira_unit`, `log`, `die`, `json_only` are reimplemented directly in Rust, not
   reached through the seam.** Unlike `bdq`/`bead_reopen`, they carry no graph semantics
   to drift from — `spira_unit` is twelve lines of systemctl calls (`gather::spira_unit`),
   `log`/`die` are one `printf` (`util::log_line`), `json_only` is a `sed` one-liner
   (`util::json_only`, unit-tested against the exact warning-banner case bd produces).
5. **`iso()` (auron.sh's timestamp formatter, `SPIRA_TZ`-aware) still shells to `date`**
   (`util::iso_display`) rather than being reimplemented with a timezone table: only the
   system's own tzdata resolves an IANA zone faithfully, and this value only ever reaches
   a bead body a human reads — low stakes, no reason to carry a timezone-database
   dependency for it.
6. **The restart-baseline arithmetic (`restart.rs`) is factored out as its own pure
   module**, not inlined in `gather.rs`, because it is the one piece of GATHER with real
   decision logic (baseline reset on a count going down, window-expiry slide) worth
   holding to its own unit tests independent of any systemctl fake.
7. **No `--sweep`-style mode, no fayth.** Auron is not a persona and claims nothing; its
   CLI is `[--home DIR] [--report]` only, unlike `aeon`'s richer grammar.

## 4. Parity evidence

See the delivery report (bead sp-zpaq0) for the `testenv` run of `test-auron.sh`
(repointed from `bash "$SH/auron.sh"` to `command auron --home "$SH"`, its fixture
otherwise unchanged) against this binary, and the wall-clock comparison of one pass
old vs new.

## 5. Tests

`cargo test -p auron` — 95+ unit tests: `state.rs` (TSV round-trip, the older short-line
spelling), `reconcile.rs` (every debounce transition: one sighting does not fire, a
confirmed raise, a flap does not duplicate, one clear sighting does not retract, a
confirmed clear, the refresh interval and its hand-closed-bead skip, db-unreachable holds
counters but never triggers), `restart.rs` (the baseline arithmetic: first-run floor, a
count going down resets, a window expiring slides, a standing burst does not re-alert),
`alerts.rs` (create, reopen-on-recur, flap-label replacement not addition, `acked`
clearing, the whole write-probe dance against a `BdOps` fake: failed re-derive does not
create, extras closed on adoption), `gather.rs` (log-tail truncation, mirror
configured/missing/present, world halt/drain parsing, `spira_unit`'s fallback chain),
`fallback.rs`/`heartbeat.rs` (JSON/env-file shape, atomic write leaves no `.tmp`), `seam.rs`
(the allowlist refuses, the snapshot parses conf.sh's resolution), `main.rs`
(`resolve_home`'s three-way fallback).
