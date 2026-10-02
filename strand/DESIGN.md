# strand — the stranded-work detector

Replaces `spira/strand.sh` and `spira/strand-classify.py` (sp-c7h4b). Same subcommands,
same output contract, one Rust binary. This document is the contract; the code satisfies
it and the unit tests are derived from it.

## 1. Intent

Report work that **exists and is not moving**, with the reason and the one action that
clears it, so that a human or Ops acts on **real stalls only**.

Every false row costs an operator triage: on 2026-09-28/29 the old detector raised six
false asks (sp-vvt04, sp-cxlmq, sp-srdjc, sp-7jail, sp-bpe4n, sp-mf03u). Each was one of
three defects, and each defect is now a rule in §4:

| ask | old verdict | why it was false | rule |
|---|---|---|---|
| sp-vvt04, sp-cxlmq | blocked-external on sp-yyltf | every named blocker was **closed**; the partition's `bd list` omits closed work, so "missing" was read as "outside the partition" | R1 |
| sp-bpe4n | blocked-external on sp-msk4h | blocker sp-7tw9h closed; dependents in progress | R1, R2 |
| sp-srdjc, sp-7jail | stuck on sp-yyltf / sp-msk4h | the blockers were **submitted** (spira-submitted), i.e. built and waiting for a round | R2 |
| sp-mf03u | blocked-external on sp-zs04v | sp-zs04v.4 closed (R1); sp-zs04v.5/.6 are deliberately ordered behind the epic sp-pswer | R1, R4 |

## 2. Contract

### 2.1 Invocation

```
strand report [--json]        human view; changes nothing but its own stdout
strand check                  timer path: act once where the fix is mechanical, escalate once where it is not
strand check --dry-run        classify and print what it would do; changes nothing
strand check --from <f|->     classify a saved classifier TSV instead of the live graph
strand throttle-state         print "open|shut|unreadable<TAB>detail"
strand -h | --help
```

As before: a first argument that is not a subcommand means `report` and is then parsed as
a flag (`strand --json` is `strand report --json`). An unknown flag prints
`<ts> spira: FATAL unknown argument '<x>' — try: report [--json] | check [--dry-run]` on
stderr and exits 1.

Exit codes: 0 on success. 1 on a usage error. 2 when the bead store could not be read
(new — the old script classified an empty graph, printed nothing and pruned the episode
state; see §4 R7).

### 2.2 Callers (read every one; see also §7 Cutover)

| caller | how | what it relies on |
|---|---|---|
| `spira/sentinel.sh:317` (CHECK 2b) | `"$SPIRA_HOME/strand.sh" check 2>&1` | stdout lines starting `RECLAIMED`, `RECOMPUTED`, `STRANDED`; exit status ignored |
| `spira/cockpit.sh:2427` `strand_keys` | reads `$SPIRA_RUN/strands.json` | keys `<partition>:<kind>:<id>` split with `rsplit(":", 2)`; values carry `escalated` |
| `auron/src/main.rs` (formerly `spira/auron.sh:108,518`, retired sp-zpaq0) | reads `$SPIRA_RUN/strands.json` (or `SPIRA_AURON_STRANDS`) | a JSON object |
| `cockpit/health.sh` | `SP_STRAND_GHOST`, `SP_STRANDS` from cockpit.sh | kind `ghost` spelled as before |
| operator / concierge / runbooks | `strand.sh report [--json]` | the text and JSON shapes in §3.4 |
| bash suites `test-strand-lock.sh`, `test-strand-reclaim-n.sh`, `test-sentinel-store-reads.sh` (case 7), `test-unit-name.sh`, `test-closed-strand.sh` | run `strand.sh` | see §7 |

### 2.3 Inputs

| input | source | notes |
|---|---|---|
| the bead store | `$SPIRA_LIST_SNAPSHOT` when readable (sentinel's `bd list --all --limit 0` of this pass), else **one** `bd -C $SPIRA_DB list --all --limit 0 --brief --json` | the WHOLE store, closed beads included. One read per run, not per partition. |
| the claimable set | `$SPIRA_READY_SNAPSHOT` when readable (filtered in-process by scope label, partition labels, exclusions), else per partition `bd ready --limit 0 --exclude-type epic,event -u [--label <scope>] [--exclude-label <no_loop label>] --label <labels> [--exclude-label <excl>] --json` | `bd ready` stays the authority on claimability |
| partitions | `SPIRA_LABELS` (+ `SPIRA_EXCLUDE_LABELS`) narrows to one; otherwise the **roster probe** (§2.5) | exclusions: `SPIRA_EXCLUDE_LABELS` overrides all, else the fayth's own, else `spira-poison,<ask>,<ci>` |
| wait holds | `lifecycle_enforce` off: beads labelled `spira-waiting-operator` in the store already read; on: `spira-lc list --hold wait` → `[{bead_id}]` | §9: off never runs spira-lc; on, unreachable ⇒ cannot tell |
| aeon liveness | `$SPIRA_RUN/hold-<id>.pid` (pid alive) and `$SPIRA_RUN/aeon-*-<id>.pid` (pid alive AND `/proc/<pid>/cmdline` contains `aeon.sh`) | never `pgrep -f` |
| aeons per partition / fleet | `systemctl --user list-units 'spira-aeon-<fayth>-*' --no-legend` (and `'spira-aeon-*'`) when `SPIRA_SUMMON` is `systemd-run` (default); else live pidfiles | read-only: dead pidfiles are no longer deleted here |
| capacity pause | first field of `$SPIRA_CAPACITY_PAUSE` (default `$SPIRA_RUN/capacity-pause`), an epoch | read-only: the probe/lift side effects of `capacity_paused` belong to the summon path |
| admission throttle | `$SPIRA_THROTTLE_STAMP` (default `$SPIRA_RUN/queue-throttled`) | exists+readable+non-empty ⇒ shut (`depth=N` parsed); exists but unreadable/empty ⇒ unreadable; absent ⇒ open |
| pass truncation | `$SPIRA_RUN/sentinel.log`: after the last `: state: open=` line (the sentinel's pass-start marker), a `CHECK7 <fayth>: not evaluated` for a fayth of the partition | |
| harness pulse (report header) | `systemctl --user is-active <unit>` for `spira-sentinel[-<instance>].timer`; mtime of sentinel.log | `?` unit ⇒ `unknown` |
| episode state | `$SPIRA_RUN/strands.json` | §3.3 |

### 2.4 Outputs and side effects

`report`: stdout only (§3.4). Reads, never writes, the episode state.

`check` (under an exclusive non-blocking `flock` on `$SPIRA_RUN/strands.json.lock`; a
second runner logs `check: another instance holds the lock — declining to avoid acting on
stale state` and exits 0):

1. classify, age every row against the state, write the pruned state (atomic: unique temp
   file + rename) — except under `--dry-run`, which writes nothing (strand.sh rewrote the
   state even when dry);
2. for each non-`info` row:
   - `--dry-run`: log `would <disp> <kind> <id> in [<part>] (<age>s, acted=<a> escalated=<e>): <detail>`;
   - `act` not yet acted: perform the mechanical fix (below), mark `acted`;
   - otherwise, not yet escalated, and (`act` already tried, or age ≥ `SPIRA_STRAND_GRACE`,
     default 900 s): escalate (mail), mark `escalated`, print `STRANDED <kind> <id> — <detail>`;
3. if anything was handled, log `check: <n> stranded item(s) handled`.

Mechanical fixes, unchanged in effect:

| kind | effect | stdout |
|---|---|---|
| `ghost` | `spira-lc show <id>` → `spira-lc event bead <id> --expect <state> --version <v> --actor strand --kind '"HolderDead"'`; one `reclaimed`/`ghost` row into `events` (`bd sql`); `bd note <id> --stdin` (fixed text); `branch.reclaimed` line in `$SPIRA_RUN/events.log` with the per-key cooldown in `$SPIRA_RUN/events/`; at exactly `SPIRA_RECLAIM_AT` (5) reclaims, a `reclaim-ceiling` escalation | `RECLAIMED <id> — <detail>` |
| `stale-blocked` | `bd recompute-blocked` | `RECOMPUTED is_blocked — <id> stuck with every blocker closed` |

Escalation: `$SPIRA_HOME/mail send operator --from 'Strand check <strand@spira>'
--subject <title> --kind question --default <action>`, body on **stdin**; titles exactly as
before (`Spira: <id> is stranded (<kind>)`, `Spira: the [<part>] queue is stranded
(<kind>)`, `Spira: <id> keeps losing its aeon — the host, not the bead`). The body is the
bead context (`bd show <id> --json`, rendered as before) + `WHY THIS IS ESCALATED` + the
last 12 sentinel log lines. No `mail` ⇒ no mail, state still marked (as before).

### 2.5 The roster probe — the one bash seam

A persona's partition is `FAYTH_LABELS`/`FAYTH_EXCLUDE_LABELS` in a `.fayth` file, which
is **bash** that interpolates `conf.sh` variables. `[persona.<name>].labels` in spira-config
does not carry the scope label or the exclusions (builder's `qa-proposed`; batcher's label
is `[]` there), so it is not yet an authority. Rather than copy the predicate, `strand`
asks the one that exists: it runs `bash -s` with a fixed script on **stdin** that sources
`<SPIRA_HOME>/lib.sh` and prints, one line per partition, `labels<TAB>exclude<TAB>fayths`
(`fayth_partitions` + `fayths_for_labels`, falling back to `spira_fayths`). No payload
travels in argv; the lib path travels in the environment (`STRAND_LIB`). Retire this when
spira-config's persona table carries labels and exclusions (§7).

### 2.6 Configuration

Each key: process environment (`conf.sh` exports most `SPIRA_*` to its children) → the
spira-config library's `[spira]` table (discovered with `spira_config::discover`) →
the `conf.sh` default. strand never parses `spira.toml` itself.

| key | env | spira-config | default |
|---|---|---|---|
| db | `SPIRA_DB` | `db` | — (refuse: never fall through to bd auto-discovery) |
| run dir | `SPIRA_RUN` | `run` | — (required) |
| harness dir | `SPIRA_HOME` | `prod` | — (required for roster probe and mail) |
| bd binary | `SPIRA_BD` | `bd` | `bd` |
| ask label | `SPIRA_ASK_LABEL` | `ask_label` | conf.sh default |
| ci label | `SPIRA_CI_LABEL` | `ci_label` | conf.sh default |
| scope label | `SPIRA_SCOPE_LABEL` | `scope_label`, else `home_repo` | empty |
| no_loop label | `SPIRA_NO_LOOP_LABEL` | `no_loop_label` | conf.sh default |
| submitted label | `SPIRA_SUBMITTED_LABEL` | `submitted_label` | `spira-submitted` |
| queue-wait label | `SPIRA_QUEUE_WAIT_LABEL` | `queue_wait_label` | `spira-queue-waiting` |
| open-children label | `SPIRA_OPEN_CHILDREN_LABEL` | `open_children_label` | `spira-open-children` |
| pool | `SPIRA_MAX_AEONS` | `max_aeons` | unset (explicit 0 ⇒ pool-paused) |
| fleet cap | `SPIRA_MAX_LIVE_AEONS` | `max_live_aeons` | 0 (unconfigured) |
| throttle release | `SPIRA_QUEUE_THROTTLE_RELEASE_AT` | `queue_throttle_release_at` | 8 |
| lifecycle switch | `SPIRA_LIFECYCLE_ENFORCE` (`1`/`true` on, anything else off) | `lifecycle_enforce` | off (§9) |
| instance | `SPIRA_INSTANCE` | `instance` | `prod` |
| grace windows | `SPIRA_GHOST_GRACE` (300), `SPIRA_STRAND_GRACE` (900), `SPIRA_RECLAIM_AT` (5), `SPIRA_EVENT_COOLDOWN` (3600), `BD_TIMEOUT` (180) | — | as shown |
| snapshots | `SPIRA_LIST_SNAPSHOT`, `SPIRA_READY_SNAPSHOT` | — | none |
| test seams | `SPIRA_SYSTEMCTL`, `SPIRA_SUMMON`, `SPIRA_CAPACITY_PAUSE`, `SPIRA_THROTTLE_STAMP`, `SPIRA_NOW` | — | |

## 3. Schema

### 3.1 Bead record (`bd list --json`, `bd show --json`)

```rust
struct Bead {
    id: String,
    title: Option<String>,
    status: Status,               // open | in_progress | blocked | deferred | closed | <other>
    issue_type: Option<String>,   // "epic" matters
    labels: Vec<String>,          // null ⇒ []
    parent: Option<String>,
    assignee: Option<String>,
    lease_expires_at: Option<String>,   // RFC 3339, "Z"
    dependencies: Vec<Dependency>,      // null ⇒ []
    // report/escalation only (bd show): priority, created_at, description, notes
}
struct Dependency {
    depends_on_id: Option<String>,      // bd list shape
    #[serde(alias = "dependency_type")] r#type: Option<String>,  // "blocks" is the only one that blocks
    id: Option<String>,                 // bd show shape: the target is `id`
}
```

Unknown fields are ignored; a missing field is its default. A `blocks` edge's target is
`depends_on_id`, or `id` in the `bd show` shape.

### 3.2 Row (classifier output, and the `--from` input)

TSV, five fields: `kind  id  disposition  detail  action`; disposition ∈ `act | escalate |
info`; tabs inside detail/action are replaced by spaces. Internally `(partition, Row)`.

Kinds: `ghost`(act) · `deferred-unescalated`(escalate) · `starved`(escalate) ·
`capacity-paused` `pool-paused` `pass-truncated` `throttled` `fleet-saturated`(info) ·
`throttle-unreadable`(escalate) · `empty`(escalate) · `waiting` `poisoned`(info) ·
`stale-blocked`(act) · `blocked-external`(escalate) · `stuck`(escalate) · **`sequenced`
(info, new)** · `cycle`(escalate). Escalation-only kind: `reclaim-ceiling`.

### 3.3 Episode state `$SPIRA_RUN/strands.json`

```json
{ "<partition>:<kind>:<id>": { "first": <epoch>, "acted": <epoch|0>, "escalated": <epoch|0> } }
```

Unchanged. Rewritten whole each `check` pass with exactly the rows seen this pass (a cleared
strand starts a fresh episode). **Not** rewritten when the store could not be read (R7).

### 3.4 Report output

Text:
```
sentinel timer: <active>   last pass: <age>s ago
<blank>
no stranded work in [<partitions space-separated>]
```
or a `%-16s %-20s %-16s %-9s %6s  %s` header (`PARTITION KIND ID DISPOSITION AGE DETAIL`),
one `%-16s %-20s %-16s %-9s %5sm  %s` line per row and a `%71s→ %s` action line.

`--json`:
```json
{ "sentinel_timer": "active", "last_pass_seconds": 42,
  "strands": [ { "partition", "kind", "id", "disposition", "age_seconds",
                 "acted_at", "escalated_at", "detail", "action" } ] }
```

## 4. Classification rules

Definitions, for one partition P (labels ⊆ bead labels) over the whole store S:

- **closed**: `status == closed`.
- **moving(b)**: `in_progress`, or labelled submitted, or labelled queue-wait, or in P's
  claimable set. (Submitted = built, awaiting a round; queue-wait = its blocker is closed
  and awaiting its landing. Both are the pipeline doing its job.)
- **parked(b)**: labelled ask, or `deferred`. **poisoned(b)**: labelled `spira-poison`.
- **delegated(b)**: labelled open-children — its deliverable is its own children.
- **progressing(t)** (recursive, memoised, a cycle reads false): closed; or moving; or an
  epic; or parked; or poisoned; or open with at least one non-closed `blocks` target that is
  progressing. Absent from S ⇒ false (unknown is never read as closed).

Rules:

- **R1 — a `blocks` edge onto a closed bead is not blocking**, wherever that bead lives.
  The store read includes closed beads, so this is a lookup, not a guess.
- **R2 — a moving dependent is not stranded.** An epic with any open child that is moving
  is skipped entirely (as before, now with submitted/queue-wait counted). A moving blocker
  makes its dependents "waiting", not "stuck".
- **R3 — blocked-external needs a non-closed, non-progressing blocker outside P holding an
  unstarted dependent.** Unstarted = open child, not moving, parked, poisoned or delegated.
  A blocker absent from the store still counts (named `(not in the store)`).
- **R4 — a `blocks` edge onto an epic is intentional sequencing.** In bd's data model an
  epic is a container, never a unit of work an aeon claims; the only reason to make a bead
  block on one is "start after that whole epic". (bd forbids a parent from blocking its own
  child, so the target is never the dependent's own epic.) Such edges produce an `info` row
  `sequenced`, never an escalation. The target epic's own stalls are reported on the target,
  in whichever partition watches it. Example: sp-zs04v.5 → sp-pswer.
- **R5 — stuck** = unstarted children whose remaining blockers are all inside P, none of
  them progressing, and no sequenced edge.
- **R6 — stale-blocked** = unstarted children with no non-closed blocker at all, yet none
  claimable → `bd recompute-blocked` once, then escalate.
- **R7 — an unreadable input is not an empty graph.** When the store, a partition's ready
  set or the roster cannot be read: no rows, no state write, a message on stderr, exit 2.
  (strand.sh classified an empty graph, printed nothing and pruned every episode, so the
  next readable pass re-escalated everything.)
- **R8 — children** are every bead in S whose `parent` is the epic. Closed children count
  toward completeness (an all-closed epic is complete, not `empty`); open children outside P
  are another partition's to watch and are not analysed here, but do count as children.
- **R9 — deferred-unescalated** is exempt when any `blocks` target anywhere in S is non-closed
  A deferred bead whose `defer_until` is in the future is a timed hold: an `info` row `held` (`held-until <ts>`), never stranded. No `defer_until`, or one already past, stays stranded.
  (the old check saw only P, so a foreign live blocker read as none).

Unchanged: ghost (in_progress, holder not alive, lease expired past grace, not ask-labelled,
not wait-held); starved and its five info variants (capacity, pool, truncation, throttle,
fleet) plus throttle-unreadable; cycle (Tarjan over P's open sub-graph).

## 5. Module layout

```
strand/src/main.rs      CLI parse, dispatch, exit codes
strand/src/model.rs     serde types: Bead, Dependency, Row, Disposition, StateEntry, ReportJson
strand/src/classify.rs  the pure classifier: Inputs → Vec<Row>   (all rules of §4; unit-tested)
strand/src/state.rs     episode state: age/prune/mark, atomic write, flock
strand/src/config.rs    §2.6 resolution
strand/src/probe.rs     the world: bd, spira-lc, /proc, systemctl, files, roster probe
strand/src/check.rs     the check loop and its effects; report rendering
strand/src/timefmt.rs   RFC 3339 parse, UTC/local formatting (no chrono)
```

## 6. Tests (unit, `cargo test -p strand`)

From the bead: closed blocker not reported; submitted dependent not reported; in_progress
dependent not reported; open external blocker on an unstarted dependent reported; empty
epic reported as empty. From tonight's asks, as fixtures built from `bd show` of the real
edges: sp-yyltf (sp-vvt04/sp-cxlmq), sp-msk4h (sp-bpe4n, sp-7jail), sp-yyltf stuck
(sp-srdjc), sp-zs04v (sp-mf03u → `sequenced`). Plus: all-closed epic is not empty; unknown
blocker still reported; stuck still reported when the chain head is dead; stale-blocked;
deferred exemption; ghost; cycle; starved variants; state aging/pruning; TSV/JSON shapes;
throttle-state parsing; `--from` parsing.

## 7. Cutover (operator's edits — not made by this bead)

| # | file:line | today | becomes |
|---|---|---|---|
| 1 | `spira/conf.sh:1096` (beside `SPIRA_LC_BIN`) | — | `: "${SPIRA_STRAND_BIN:=$(spira_bin strand 2>/dev/null)}"`, and `SPIRA_STRAND_BIN` added to the export list (conf.sh:2483 block) |
| 2 | `spira/sentinel.sh:317` | `stranded="$("$SPIRA_HOME/strand.sh" check 2>&1)"` | `stranded="$("$SPIRA_STRAND_BIN" check 2>&1)"` — lines 318–321 (the `^RECLAIMED/RECOMPUTED/STRANDED` counts) unchanged |
| 3 | `Makefile:46` and `Makefile:53` | `for _b in loom broker … reconciler-flow; do` / `… reconciler-flow panel; do` | add `strand` to both lists |
| 4 | `spira/strand.sh`, `spira/strand-classify.py` | the component | delete |
| 5 | `spira/test-strand-lock.sh:85,126,135,165,190,215`, `spira/test-strand-reclaim-n.sh:58` | `bash "$HERE/strand.sh" check --from …` | `"$SPIRA_STRAND_BIN" check --from …` (same env: `SPIRA_RUN`, `SPIRA_HOME`, `SPIRA_LABELS`, `SPIRA_STRAND_GRACE`) — or retire in favour of `state::tests` / `check::tests` |
| 6 | `spira/test-strand-classify.sh` | runs strand-classify.py on fixtures | retire; its cases are `classify::tests` |
| 7 | `spira/test-sentinel-store-reads.sh:255,266` (case 7) | `bash "$HERE/strand.sh" report` counting bd calls | point at the binary; 7b holds (0 bd calls with both snapshots); 7a's positive control still holds (1 `bd list` + 1 `bd ready` per partition) |
| 8 | `spira/test-unit-name.sh:49,88–91` | awk-extracts `harness_state` from strand.sh | drop the strand.sh case; `probe::spira_unit`/`harness_state` carry it (a unit test there needs a stub systemctl) |
| 9 | `spira/test-closed-strand.sh:46` | `stub strand.sh 'exit 0'` | stub the binary via `SPIRA_STRAND_BIN` |
| 10 | `spira/auron.sh:92` (comment) | names strand-classify.py | **MOOT.** auron.sh is retired (sp-zpaq0); the comment does not exist to reword |
| 11 | later | roster probe (§2.5) | retire when `[persona.<name>]` carries labels and exclusions |

This branch also carries `a21b071a1` (an aeon's earlier bash fix of the same defect in
strand.sh/strand-classify.py/test-strand-classify.sh). It is superseded by this crate and
dies with row 4; the operator may keep it as the interim fix until the cutover lands.

## 9. Lifecycle switch

**Finding (operator, 2026-09-28):** the lifecycle machine was never deployed on this host:
no `spira_lifecycle` database, no `spira_lc` grant, no service or socket. **Decision:**
`lifecycle_enforce` is THE switch for everything that touches the lifecycle machine.

**Resolution** (`Config::resolve` → `spira_config::resolve_lifecycle_enforce`, the aeon
crate's rule): `SPIRA_LIFECYCLE_ENFORCE` in the environment wins (`1`/`true` on, anything
else, including empty, off); else the typed `spira.lifecycle_enforce`; else **off**. `spira-lc`
is invoked by name on the launcher's PATH (sp-gypjk), and its presence is never an input: a
release always carries it, and it does not turn anything on.

| | **off** (production today) | **on** |
|---|---|---|
| spira-lc | **never run** | `list --hold wait`, `show`, `event … HolderDead` |
| ghost wait exemption | beads labelled `spira-waiting-operator` (pre-sp-i2m7y `SPIRA_RECLAIM_SKIP_LABEL` default), read off the store this pass already loaded | the spira-lc `wait` holds; an unset/non-executable binary, a failed call or an unparseable reply is an `Err` — the pass is "cannot tell" (R7), never an empty set that would reclaim a held bead |
| ghost fix, first step | `bd reclaim --id <id> --older-than 1s [--label <partition>]` (pre-sp-i2m7y strand.sh; no `--label` for partition `-`); a failure is a `WARN` | `spira-lc HolderDead` (show, then the CAS event). Stays best-effort so the counter/note/event still run, but every miss — unset or non-executable binary, a failed show or event, no row — is a loud `WARN … lifecycle_enforce is on and spira-lc HolderDead did not happen` |
| ghost row's action text | `bd reclaim --id <id>` | `spira-lc HolderDead <id>` |
| rest of the ghost fix (reclaim counter, note, event, ceiling escalation) | unchanged | unchanged |

**Tests:** `config::lifecycle_switch_env_then_toml_then_off`,
`check::off_ghost_fix_is_bd_reclaim_and_never_runs_spira_lc` and
`check::off_wait_exemption_is_the_legacy_label_and_never_runs_spira_lc` (an executable
recorder stands in for spira-lc and must never be called),
`check::on_runs_spira_lc_and_unreachable_is_an_error_not_an_empty_set`,
`classify::ghost_needs_an_expired_lease_and_no_holder` (both action texts).

**Cutover addition:** the sentinel passes `SPIRA_LIFECYCLE_ENFORCE=0|1` to strand; nothing
else is needed. If strand is ever run outside conf.sh with no environment value, it reads
`spira.lifecycle_enforce`, default off.
