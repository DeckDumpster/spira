# landing-pass — pr-mode landing on a short timer

## 1. Intent

This crate lands closed beads' `spira/<id>` branches for pr-mode repositories. It runs on a
short timer and has no local gate. For each branch it:
- records `CONTENT` when base already contains every change;
- defers while a live aeon still holds the bead;
- otherwise delegates to `pr-pass-branch.sh` (open or refresh the PR, or record
  merged/closed).

## 2. Contract

- **In:** `SPIRA_HOME`, `SPIRA_RUN` (landstate, lock, log), `SPIRA_DB`, `SPIRA_REPO_MAP`,
  `SPIRA_PR_PASS_BRANCH_SH`, `SPIRA_LC_BIN` (read only when the switch is on, §3), and
  `spira.lifecycle_enforce` through spira-config when the environment does not pin it.
- **Out:** landstate records, `landing-pass.log`, and `pr-pass-branch.sh`'s own effects.
  When the switch is on, it also sends the delivery machine's `Delivered` event for a
  content-proven merge.
- One pass at a time (flock on `landing-pass.lock`).

## 3. Lifecycle switch

**Finding (operator, 2026-09-28):** the lifecycle machine was never deployed on this host.
There is no `spira_lifecycle` database, no `spira_lc` grant, and no service or socket.
**Decision:** `lifecycle_enforce` is THE switch for everything that touches the lifecycle
machine.

**Resolution** (`spira_config::lifecycle_enforce`, the aeon crate's rule):
- `SPIRA_LIFECYCLE_ENFORCE` wins: `1`/`true` is on, and anything else, including empty, is off.
- Else `spira.lifecycle_enforce`.
- Else **off**.

A `spira-lc` under `bin/` or `target/release/` never turns it on.

| | **off** (production today) | **on** |
|---|---|---|
| spira-lc | **never run** | `show <id>`, then `event delivery … Delivered` |
| content already on base | the `CONTENT` landstate record, and nothing else: pre-sp-n1ilm behaviour | the same, plus `Delivered` when the delivery row is `PR_OPEN`. This path is **best-effort additive**, as sp-n1ilm designed it. No delivery row, or a row not in `PR_OPEN`, is the ordinary case and stays quiet. A spira-lc that cannot start, exits non-zero, answers unparseably, or refuses the event is a loud `LIFECYCLE:` line on stderr, and the pass continues |
| everything else in the pass | unchanged | unchanged |

**Tests** (`lifecycle_tests`):
- `off_never_runs_spira_lc`: an executable recording spira-lc must never be called.
- `on_delivers_a_pr_open_row`
- `on_no_delivery_row_is_the_ordinary_quiet_case`
- `on_unreachable_or_refusing_machine_is_loud`

**Outside this crate:** `pr-pass-branch.sh` calls `lc_deliver_pr_merged` and
`lc_deliver_pr_closed` (lc.sh) itself, for exits 7 and 8. That bash is not governed by this
crate's switch, so it must honour `SPIRA_LIFECYCLE_ENFORCE` in its own cutover.
landing-pass passes its environment through unchanged.
