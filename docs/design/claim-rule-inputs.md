# The claim rule's inputs

What `spira-claim/src/rank.rs` `claimable` reads to decide whether a bead is claimable, where
it reads each input, and what writes it. Read from `local/main`. The rule is `spira-claim/src/rank.rs`
`stack_plan`; `spira-claim/src/cli.rs` `judge_rows` gathers its inputs and applies the
filters that run after it (`retain_unstopped`, `retain_uncarried`).

Stores named in the third column:

- **lifecycle** — the bead's lifecycle record, parsed by `spira-claim/src/rank.rs` `parse_lifecycle` from `spira-lc list`.
  Every lifecycle write is an event applied by `lifecycle/src/bead.rs` `apply`.
- **readiness** — the `readiness/<id>` record of the control store (`spira-lc/src/control/records.rs` `Readiness`). The rule does not read it; it is listed only where it matters (`blocked_by` below).
- **content** — the planning plane's rows, read by `spira-claim/src/store.rs` `items_by_ids` (`spira-lc plan items`) into `spira-claim/src/rank.rs` `ReadyRow`.
- **file** — a file under the run or queue directory.
- **config** — a `spira.<key>` setting, loaded by `spira-claim/src/store.rs` `load_config`. Set with `spira-config set spira.<key>`; defaults live in `spira/conf.d/SPIRA_<KEY>`.

## Table

### Candidate lifecycle fields

| Input | Read by | Source today | Written by |
|---|---|---|---|
| candidate `state` (must be READY or REWORK) | `spira-claim/src/rank.rs` `stack_plan` (`Verdict::OwnState`); `spira-claim/src/ready.rs` `lifecycle_ready_ids` | lifecycle | `lifecycle/src/bead.rs` `apply` through `spira-lc/src/main.rs` `apply_bead_event`: `spira-lc create-bead` (`bead/src/lc.rs` `create_row`), `work submit` (`spira-lc/src/work.rs` `cmd_submit`), `spira-lc release`, `spira-lc returned`, `spira-lc/src/callers.rs` `run`, the landing pass |
| candidate `holds` (any hold but `wait` refuses) | `spira-claim/src/rank.rs` `parse_lifecycle`, `stack_plan` (`Verdict::Held`) | lifecycle | `spira-lc hold` / `spira-lc unhold` (`spira-lc/src/callers.rs` `run`); `work blocked` (`spira-lc/src/work.rs` `cmd_blocked`) |
| candidate snooze (`snoozed_until`, from `reason` while the `wait` hold is set) | `spira-claim/src/rank.rs` `parse_lifecycle`, `stack_plan` (`Verdict::Snoozed`) | lifecycle (`reason` of the `wait` hold) | `spira-lc hold <id> wait snooze-until:<epoch>`; `dwarf/src/verdict.rs` `hold_unless_terminal` with `spira-store/src/lc_state.rs` `snooze_reason`, called from `dwarf/src/teardown.rs` |
| candidate `stack` (the stack recorded at its last claim) | `spira-claim/src/rank.rs` `parse_lifecycle`, `stack_plan` (`rework_claim_stack_unchanged`, `Verdict::StackUnchanged`) | lifecycle | the `Claim` event (`lifecycle/src/bead.rs` `BeadEventKind`), sent from `dwarf/src/run.rs` `claim` through the `lc_claim_bead` seam call (`dwarf/src/seam.rs`); `lifecycle/src/bead.rs` `PrereqLanded` and `BaseWithdrawn` edit it |
| candidate `reason` (why it last left the queue) | `spira-claim/src/rank.rs` `parse_lifecycle`, `stack_plan` | lifecycle | the same events as `state`; each sets `reason` as it applies (`lifecycle/src/bead.rs` `apply`) |

### Blocker lifecycle fields

A blocker is the candidate's parent and each `blocks` target (`spira-claim/src/rank.rs` `blockers`).

| Input | Read by | Source today | Written by |
|---|---|---|---|
| blocker `state` and `reason` (LANDED/DONE pass; CERTIFIED/IN_DELIVERY stack, except an IN_DELIVERY row whose `reason` is `delivered-from-submitted` (`lifecycle/src/bead.rs` `provisional`), which blocks; terminal passes; the rest block) | `spira-claim/src/rank.rs` `stack_plan`, `parent_exempt` | lifecycle (read by `spira-claim/src/store.rs` `lifecycle_of`) | as the candidate's `state`: `work submit`, `spira-lc certify`, `spira-lc deliver`, and the landing pass's event in `lifecycle/src/bead.rs` `Delivered` |
| blocker `tip` (the certified tip stacked on; parent exemption also reads whether a tip exists) | `spira-claim/src/rank.rs` `parse_lifecycle`, `stack_plan`, `parent_exempt` | lifecycle | events in `lifecycle/src/bead.rs` `Submit`, `GatePass` and `Requeued`, sent by `spira-lc/src/work.rs` `cmd_submit` and `spira-lc/src/callers.rs` `run` |
| blocker `stack_depth` (deepest stacked blocker + 1 is the depth) | `spira-claim/src/rank.rs` `parse_lifecycle`, `stack_plan` | lifecycle | the `Claim` event's `stack_depth`, sent by `dwarf/src/run.rs` `claim` from the proposal `spira-claim/src/cli.rs` `cmd_stack` printed |
| blocker `stack_conflict` (a stacked base failed to merge this tip) | `spira-claim/src/rank.rs` `mark_stack_conflicts`, `stack_plan` | file (see below), applied to CERTIFIED / IN_DELIVERY rows only | see "Files under run or queue" |

### Candidate content fields

| Input | Read by | Source today | Written by |
|---|---|---|---|
| `parent` | `spira-claim/src/rank.rs` `blockers`, `stack_plan`, `parent_exempt` | content | `bead.sh file --parent <id>` (`bead/src/filing.rs` `create`); `spira-lc/src/work.rs` `cmd_file` (`work file-followup`, `work split`; `work split --prerequisite` writes none); `bd update --parent` through `spira-lc/src/bd.rs` `write` |
| `dependencies`, `blocks` edges only | `spira-claim/src/rank.rs` `declared_blockers` (by `spira_config::edges::blocks`) | content (`dependencies`) | `bead.sh dep add <id> <dep> [--type blocks]` (`spira-lc/src/bd.rs` `add_blocker`, then the content door's own mirror); `bead.sh dep remove`; `bd dep add` / `bd dep rm` through `spira-lc/src/bd.rs` `write` |
| `issue_type` of the candidate (epic and event are not claimed) | `spira-claim/src/ready.rs` `in_scope` | content | `bead.sh file --kind <kind>` (`bead/src/filing.rs` `create`); `spira-lc create-bead` refuses a type with no lifecycle record (`spira-lc/src/cutover.rs`) |
| `labels` — `repo:<name>` (the stackable check compares it with each blocker's; the carried and stopped checks read it) | `spira-claim/src/rank.rs` `stack_plan` (`label_value("repo:")`), `load_integrations`; `spira-claim/src/cli.rs` `retain_unstopped` | content (`labels`) | `bead.sh file --repo <name>` (`bead/src/filing.rs` `create`); `bead.sh label add <id> repo:<name>` (`bead/src/door.rs` `label_add`, then `bead/src/lc.rs` `set_repo`); `spira-lc set-repo`; `spira-lc/src/work.rs` `cmd_file` |
| other `labels` | see "Labels the rule reads" | content | `bead.sh label add|remove <id> <label>` (`bead/src/door.rs` `label_add`); `spira-lc content update --add-label/--remove-label` |

The candidate's `status` is not read: lifecycle `state` is the claim (`spira-claim/src/ready.rs` `lifecycle_ready_ids`).

### Blocker content fields

Read only for a blocker with no lifecycle record, or a CERTIFIED / IN_DELIVERY one (`spira-claim/src/cli.rs` `judge_rows`, the `missing` set).

| Input | Read by | Source today | Written by |
|---|---|---|---|
| blocker `issue_type` (a work type may stack; a non-work type waits on its status) | `spira-claim/src/rank.rs` `is_work`, `spira_config::edges::blocker_finished` (`spira_config::nonwork::is_work_type`) | content | `bead.sh file --kind <kind>` (`bead/src/filing.rs` `create`) |
| blocker `status` (rowless epic / decision / event / molecule / gate: closed or not) | `spira-claim/src/rank.rs` `spira_config::edges::blocker_finished` (`spira_config::nonwork::is_closed`) | content | `spira-lc close` and `spira-lc close-epic`; `bd update --status` through `spira-lc/src/bd.rs` `write` |
| blocker `repo:` label (the stackable check: same repo as the candidate) | `spira-claim/src/rank.rs` `stack_plan` | content | as the candidate's `repo:` label |

### Files under run or queue

| Input | Read by | Source today | Written by |
|---|---|---|---|
| stack-conflict marker per `(prerequisite, tip)` | `spira-claim/src/rank.rs` `mark_stack_conflicts` → `spira_config::stack_conflict::holds` | file under `$SPIRA_RUN` | `spira-config/src/stack_conflict.rs` `record`, called by `dwarf/src/teardown.rs` `wait_for_blocker` and `dwarf/src/run.rs` `claim` when a stacked base fails to merge |
| the integrations record, one line per integration bead: `<id> <member>...` | `spira-claim/src/rank.rs` `load_integrations`, `carrier`; `spira-claim/src/cli.rs` `retain_uncarried` | file `<queue_dir>/<repo>/integrations` | `queue/src/ops/integrate.rs` `file` (appends; `RECORD`); `land_carried` (removes) |
| scoped stops: a repo whose `claims` lane is stopped | `spira-claim/src/cli.rs` `retain_unstopped` → `spira_config::scoped_stop::stop_for` | file under `$SPIRA_RUN` | `spira-config/src/scoped_stop.rs` `pause_repo` and `pause_repo_typed`, reached by `spira world pause --repo` (`spira-world/src/bin/world.rs` `cmd_pause`) and the queue stop (`queue/src/ops/round.rs` `stop`) |
| reservations: a reserved bead id is not claimed | `spira-claim/src/cli.rs` `retain_unstopped` → `spira_config::scoped_stop::reserved_ids` | file under `$SPIRA_RUN` | `spira-config/src/scoped_stop.rs` `reserve`, reached by `spira world pause --reserve <ids>` (`spira-world/src/bin/world.rs` `cmd_pause`) |

### Config keys

| Input | Read by | Source today | Written by |
|---|---|---|---|
| `stack_max_depth` (clamped to `spira-claim/src/rank.rs` `STACK_CEILING`; 0 turns stacking off) | `spira-claim/src/cli.rs` `judge_rows`; `spira-claim/src/rank.rs` `stack_cap`, `stack_plan` | config `spira.stack_max_depth` | `spira-config set spira.stack_max_depth`; default in `spira/conf.d/SPIRA_STACK_MAX_DEPTH` |
| `incident_label` | `spira-claim/src/rank.rs` `stack_cap` | config `spira.incident_label` | `spira-config set spira.incident_label`; default in `spira/conf.d/SPIRA_INCIDENT_LABEL` |
| `pilgrim_label` | `spira-claim/src/rank.rs` `stack_cap`, `spira-lc/src/control/claimable.rs` `judge` | config `spira.pilgrim_label` | `spira-config set spira.pilgrim_label`; default in `spira/conf.d/SPIRA_PILGRIM_LABEL` |
| `skuld_label` | `spira-claim/src/rank.rs` `stack_cap`, `spira-lc/src/control/claimable.rs` `judge` | config `spira.skuld_label` | `spira-config set spira.skuld_label`; default in `spira/conf.d/SPIRA_SKULD_LABEL` |
| `scope_label` | `spira-claim/src/cli.rs` `scope_label` → `spira-claim/src/ready.rs` `in_scope` | config `spira.scope_label` | `spira-config set spira.scope_label`; default in `spira/conf.d/SPIRA_SCOPE_LABEL` |
| `no_loop_label` | `spira-claim/src/cli.rs` `no_loop_label` → `spira-claim/src/ready.rs` `in_scope` | config `spira.no_loop_label` | `spira-config set spira.no_loop_label`; default in `spira/conf.d/SPIRA_NO_LOOP_LABEL` |
| `run` (where the stack-conflict and scoped-stop files are) | `spira-claim/src/cli.rs` `judge_rows` | config `spira.run` | `spira-config set spira.run`; default in `spira/conf.d/SPIRA_RUN` |
| `queue_dir` (where the integrations file is) | `spira-claim/src/cli.rs` `retain_uncarried` | config `spira.queue_dir` | `spira-config set spira.queue_dir`; default in `spira/conf.d/SPIRA_QUEUE_DIR` |
| `runes` and `queue_wait_label` (rune attribution only; they never make a bead unclaimable) | `spira-claim/src/cli.rs` `queue_wait_label`, `bulk-ready-by-rune`; `spira-claim/src/ready.rs` `in_any_partition` | config `spira.runes`, `spira.queue_wait_label` | `spira-config set`; defaults in `spira/conf.d/SPIRA_RUNES`, `spira/conf.d/SPIRA_QUEUE_WAIT_LABEL` |

## `blocked_by`

`blocked_by` is neither of the design's two options. It is not on the lifecycle record. It lives
on `readiness/<id>` (`spira-lc/src/control/records.rs` `Readiness`), and `spira-lc/src/control/state.rs`
`column` joins it into `spira-lc list` and `show`. On `show`, `blocking` filters it down to the
prerequisites that are still blocking.

Three writers, each replacing or editing the whole list:

1. **`publish_readiness`** (`spira-lc/src/control/store/readiness.rs`). Every publish pass overwrites it with `Judged.blocked_by`, which `spira-claim/src/readiness.rs` `payload` sets to the verdict's `blocker` for a record the claim rule judged: at most one id, the first blocker the rule stopped at, or the carrying integration. For a live record the rule did not judge it is every blocker not yet finished.
2. **`mark_rejudge`** (`spira-lc/src/control/store/readiness.rs` `Store::mark_rejudge`, reached from `spira-lc/src/bd.rs` `mark_rejudge`). After a planning write it replaces the list: with the one new dependency on `dep add`, and with an empty list on `dep rm`, a priority, parent, status or defer update, or a `split-child` relabel (`spira_store::planning::claim_input_change`). It does nothing for a record no pass has judged.
3. **`set_edge`** (`spira-lc/src/control/store/direct.rs` `Store::set_edge`). Adds or removes one id. Reached from `bead.sh dep add|remove` → the content door → `spira-lc dep-add|dep-remove` (`spira-lc/src/control/state/rounds.rs` `edge`).

So between passes it is a mix of all three, and after a pass it holds only the last judged
blocker. `Readiness.gates` is keyed by every blocker the last pass read (the parent plus the
`blocks` targets), but it is a pass output, not an edge store.

**Conclusion.** Neither `blocked_by` nor `gates` is a dependency-edge store. The full and current
edge set exists only in the content store's `dependencies` table. Item 2 must write the full
`blocks` edge set as a new field.

## Labels the rule reads

Beyond `repo:`; the claim rule does not read persona.

- `split-child` (`spira_config::nonwork::SPLIT_LABEL`) — read by `spira-claim/src/rank.rs` `parent_exempt`: a child without it is not held back by a READY, tipless parent, a split child is. `dep add <child> <its parent>` writes it (`spira-lc/src/content.rs` `own_parent_wait`), and `dep remove` takes it off. Relabelling it triggers `spira-lc/src/bd.rs` `mark_rejudge`.
- The incident, pilgrim and Skuld labels (config `incident_label`, `pilgrim_label`, `skuld_label`) — read by `spira-claim/src/rank.rs` `stack_cap` and `spira-lc/src/control/claimable.rs` `judge`: a candidate carrying any of them never stacks (depth cap 0).
- The scope label and the no-loop label (config `scope_label`, `no_loop_label`) — read by `spira-claim/src/ready.rs` `in_scope` through `labels_match`: the candidate must carry every scope label and no no-loop label. Both are comma-separated lists.
- The rune predicate labels (`RUNE_LABELS` / `RUNE_EXCLUDE_LABELS` of each rune) and the queue-wait label — read by `spira-claim/src/ready.rs` `in_any_partition`. **Rune attribution only**; they never change a verdict.
- `branch:` — read by `spira-claim/src/rank.rs` `top_tier`. **Ranking only**; it does not decide claimability.

Only the `split-child` label change reaches `mark_rejudge`; relabelling to or from the incident, scope or no-loop label does not mark the record for re-judgement.
