# Test plan — Dispatch and bead contract (`dispatch`)

Part of [[test-plan-2026-09-23]], section 5. Area id `dispatch`; use-case ids are `UC-dispatch-NN`.

Scope: from "a bead exists in the store" to "an aeon is summoned for it". This covers the bead metadata contract (`bead.sh file|lint`, `repo:` and scope labels, GitHub intake), the persona roster and `FAYTH_LABELS` predicates, free-slot and lane-cap arithmetic (`fayth_free`, `summon_fayth`), drain and halt gating, the CHECK 7 fill loop, CHECK 7c unclaimable detection, the CHECK 8 starvation trigger, and ghost-lease reclaim protection (CHECK 2 / `strand-classify.py`).

Primary files (21, 240 suite-seconds on main-push run 35947142904): test-bd-stdin, test-bead-file-kinds, test-bead-lane-guard, test-bead-lint, test-check8-progressed, test-drain-expiry, test-effective-lanes, test-elastic-ceiling, test-fayth-free, test-fayth-predicates, test-fayth, test-gh-intake, test-lane-ceiling, test-lanes, test-reclaim-escalated, test-reclaim-needs-ryan, test-sentinel-capacity, test-sentinel-order, test-unclaimable-bead, test-unclaimable-cycle, test-unmapped-repo-park.
Secondary files (5, cited but costed in their own areas): test-cockpit-unclaimable (3 s), test-fayth-project-instructions (2 s), test-id-prefix (1 s), test-ops-allowlist (6 s), test-spike (24 s).

Code under test: `spira/lib.sh` (`spira_fayths`, `spira_lane_fayths`, `spira_task_fayths`, `fayth_exclude`, `fayth_ready` l.925, `fayth_free` l.1188, `summon_fayth` l.1275, `check2_protect_waiting` l.806, `detect_unclaimable_ready` l.3391, `file_unclaimable_incidents` l.3541, `_spira_lane_diag` l.4124, `spira_repo_lanes` l.4032), `spira/sentinel.sh` (CHECK 2/2c l.179–275, CHECK 7 l.1170–1275, CHECK 7c l.1333, CHECK 8 l.1385), `spira/bead.sh`, `spira/escape.sh`, `spira/strand-classify.py`, `spira/gh-intake.sh`, `spira/chamber/*.fayth`.

---

## 1. Intent (the de facto spec)

Each sentinel pass asks every persona in `chamber/*.fayth` whether it has ready work, and it asks through that persona's own `FAYTH_LABELS` predicate, which is built from configured `$SPIRA_*_LABEL` variables and never from literals. A `fayth:<name>` label can narrow the predicate but never widen it. The pass then summons as many aeons as the numbers allow. Lane personas draw first and sit outside the task pool, bounded by a collective lane cap. Task personas share `SPIRA_MAX_AEONS`. `SPIRA_MAX_LIVE_AEONS` is a fleet ceiling over both, and the last slot goes to a non-elastic or lane persona. No aeon is summoned while the world is halted or drained (a drain expires loudly), or while the account is out of capacity. A bead reaches the store only through `bead.sh`, which derives partition labels from the persona and refuses repos whose lanes do not admit it. `bead.sh lint` and CHECK 7c report beads that nobody can claim, and they never report their own reports. A lease is reclaimed from a dead worker unless the bead is waiting on Ryan. A pass that cannot read the database fails loudly and never reports "goal reached". Judgement (CHECK 8) fires only when nothing *progressed*, never merely because the pass *acted*.

---

## 2. Use cases

Dimensions use the taxonomy ids: correctness, fail-closed, observability, idempotency, concurrency, recovery, config-compat, contract, performance, test-integrity.
Tiers: T0 static, T1 unit, T2 component, T3 integration, T4 acceptance. "cert" means the certification/local gate, "batch" means batch CI, and "main" means main-push CI.

| id | requirement | dimensions | tier / where |
|---|---|---|---|
| UC-dispatch-01 | `bead.sh file --for <persona> --repo <r>` files a work bead whose labels are exactly the persona's `FAYTH_LABELS` plus `repo:<r>`, with no assignee. Missing `--for`, missing `--repo`, an unknown persona, a persona with no labels, and `--kind` with `--for` together each exit 2 without calling `bd create`. | correctness, fail-closed, contract | T1 / cert |
| UC-dispatch-02 | Non-work kinds (event, escalation, proposal, insight) map to their `bd --type`, carry the scope label and no partition label, and get `repo:` only when one is given. An insight is created closed at P4 unless a priority is given. `gate` and unknown kinds are refused, and the refusal names the kind. | correctness, contract | T1 / cert |
| UC-dispatch-03 | A persona can file only against a repo whose repo-map lanes admit its partition. The refusal exits 2 and names the repo, the lane, the repo-map, and the `SPIRA_BEAD_LANE_OVERRIDE` override. The plan lane is always admitted. | fail-closed, observability, config-compat | T1 / cert |
| UC-dispatch-04 | `bead.sh lint` reports each stored bead that lacks `repo:` or a partition label, unless the bead is an event or `no-loop`. It reports an unreadable id as *unreadable*, not as a label defect. `--all` lists exactly the offenders, and the exit code is 1 iff any bead is reported. | correctness, fail-closed, observability | T1 (judge over canned JSON) + one T2 row (real `bd show --json` shape) / cert + batch |
| UC-dispatch-05 | No harness script or chamber brief passes a bare `-` body to `bd note` or `bd create -d`. | contract | T0 / cert |
| UC-dispatch-06 | GitHub intake never mutates GitHub and never sends a credential. It excludes PRs and never fetches comments. OWNER, MEMBER and COLLABORATOR issues become work beads with live partition labels, `repo:`, `external-ref github:<repo>#<n>` and the configured priority. Other associations go to the `gh-untrusted` digest. `spira:accept` promotes an issue only when the labelling actor has write access. A rerun ingests nothing twice, even for closed beads. `--dry-run` writes nothing. A malformed priority or a label-less create fails and names the cause. Unconfigured intake exits 0 without reaching the API. | fail-closed, idempotency, contract, correctness | T1 triage/dedup + T2 script-with-stubbed-curl/bd; token greps T0 / cert + batch |
| UC-dispatch-07 | The roster is discovered from `chamber/*.fayth`. Personas that declare `FAYTH_LANE` are lane personas, the rest are task personas, and no persona is both. Operator personas (`FAYTH_SUMMON!=auto`) are in neither pool. The shipped ops persona is a lane. | correctness, config-compat | T1 / cert |
| UC-dispatch-08 | Every auto-summoned persona resolves to a non-empty predicate built from `$SPIRA_*_LABEL`, and an overridden label propagates (for example `SPIRA_PLAN_LABEL=work`). `schema.sh name` exits 2 on an undeclared key. `fayth_exclude` adds every other persona's `fayth:<name>` and `SPIRA_QUEUE_WAIT_LABEL` to each predicate. | config-compat, fail-closed, contract | T1 / cert (literal scan: T0) |
| UC-dispatch-09 | `summon_fayth` asks each persona's own predicate. Ops is summoned for incident work while plan work is at 0, and the reverse also holds. The spike bead is in the spike partition only. Poisoned, ask-labelled and escalation beads are in no partition. | correctness | T1 (stub `ready_count`) + partition rows T2 on real `bd ready` / cert + batch |
| UC-dispatch-10 | The `summon_fayth` refusal ladder runs in order, and each refusal logs its actual reason: halted, then drained, then account out of capacity, then fleet ceiling reached, then elastic last-slot reservation, then collective lane cap, then task last slot held for lane work, then nothing ready, then zero free (at concurrency cap). Each unset key keeps today's behaviour. | correctness, observability, config-compat | T1 table / cert |
| UC-dispatch-11 | `fayth_free`: an elastic persona gets the pool remainder, without subtracting running aeons a second time. With no pool it falls back to its own cap minus running. A non-elastic persona gets its cap minus running, and the pool clamps it only downward. | correctness, config-compat | T1 table / cert |
| UC-dispatch-12 | A live drain gates every summon. An expired drain is lifted in `summon_fayth` with `DRAIN EXPIRED … did not resume`, and the stamp is removed. A stamp without an `expires` line expires at mtime + `SPIRA_DRAIN_TTL`. `world.sh drain --for N` writes the deadline, so writer and reader stay pinned together. | recovery, observability, contract | T1 / cert |
| UC-dispatch-13 | Effective lanes for a repo are the intersection of `.spira/modes`, the repo-map `lanes` column and `SPIRA_FAYTHS`. Every refused lane names the source that refused it. If no source is present the result is plan only. A missing working copy counts as no modes. A modes file that does not parse logs `modes-error` and falls back. | config-compat, observability, fail-closed | T1 / cert |
| UC-dispatch-14 | CHECK 7 draws lanes first, in round-robin order persisted in `$SPIRA_RUN/lane-round-robin`. It then fills the task pool in roster order until the pool is empty, while each persona is capped per pass at `SPIRA_MAX_LIVE_AEONS` (default 4) when no pool is set. A throttle stamp holds the task pool at 0 but not the lanes, unless `SPIRA_QUEUE_THROTTLE_OVERRIDE=off`. A persona left unevaluated when the pass budget runs out is logged as `not evaluated`. CHECK 7 runs before the Sending. | correctness, performance, observability | T1 (extract `ck7_plan`) + one T3 pass / cert + batch |
| UC-dispatch-15 | `escape.sh <fayth>` summons directly when its partition has ready work, bypasses the pool and lane caps, and summons nothing when nothing is ready. It honours capacity pause. (Halt/drain: see Gap G8.) | correctness, fail-closed | T1 / cert |
| UC-dispatch-16 | CHECK 7c reports a ready bead that no persona can claim. The report names the `fayth:` preference and the reason for rejection, or names the missing partition. The check files one P1 `UNCLAIMABLE:` incident for each such bead, deduplicated on `unclaimable:<id>`. Poisoned, ask-labelled, `no-loop` and parked-persona beads are excluded. An empty queue produces no output. The detector never reports its own incidents. | correctness, idempotency, observability, fail-closed | T1 table over fixture JSON + one T2 `bd ready` shape row / cert + batch |
| UC-dispatch-17 | The cockpit's NEXT attribution names the persona that will actually claim a bead, or `unclaimable`, and agrees with UC-16. | contract, observability | T1 / cert |
| UC-dispatch-18 | CHECK 8 fires (logs `STARVED` and calls `reflect.sh`) only when plan in-progress = 0, open > 0, plan ready = 0 and progressed = 0. `acted > 0` never suppresses it. Firing is limited by the `INFERENCE_EVERY` cooldown. | correctness, observability | T1 (extract predicate) / cert |
| UC-dispatch-19 | A sentinel pass that cannot read the database exits 1 and logs `DATABASE UNREADABLE`, never `goal reached`. | fail-closed, observability | T1 (failing bd shim) + covered in the T3 pass / cert + batch |
| UC-dispatch-20 | A stale lease in every persona's partition (`fayth_partitions`) is reclaimed and each reclaim is charged through `bump_reclaim`. A pass with no partitions says so. Orphaned claims (CHECK 2c) are released. | recovery, config-compat, observability | T1 (stub `bdq reclaim` output parse) + T3 / batch |
| UC-dispatch-21 | The ghost classifier never reclaims a bead that carries the ask or reclaim-skip label. `check2_protect_waiting` adds the skip label while the bead's only open dependency is an ask, and removes it when that dependency closes. The chain from the DB through JSON to the classifier holds on real bd. | recovery, correctness, contract | T1 classifier table + one T2 chain / cert + batch |
| UC-dispatch-22 | A bead whose `repo:` is not in the repo-map is parked by `aeon.sh` with ask and overseer labels before its claim is released, so `bd ready` stops offering it and the decisions pane sees it. | recovery, observability | T1 (stub bd on the park function) / cert |
| UC-dispatch-23 | Summoning passes `CPUQuota=${SPIRA_AEON_CPU_QUOTA:-70%}` and the fayth's `FAYTH_TIMEOUT_SECONDS`. Launch flags follow `FAYTH_PROJECT_INSTRUCTIONS` in both sweep mode and bead mode. | config-compat, contract | T1 argv builder / cert |
| UC-dispatch-24 | Ops tools are an allowlist of `Bash(pattern)` entries, never bare `Bash`. Diagnostics are allowed. Writes and systemd mutations are not. | fail-closed, contract | T0 (bare-Bash scan) + T1 matcher / cert |

Machine-readable declarations live in `docs/test-plan/dispatch.toml` (schema: `test-plan/schema/catalogue.schema.json`), read by `spira/plan-lint.sh`.

---

## 3. Coverage map

Cost is ci_secs from the main-push run. When a file covers several use cases, its cost is listed on its main row and shown as "(shared)" elsewhere.

| UC | existing tests (file::case) | level & cost now | verdict |
|---|---|---|---|
| 01 | test-bead-file-kinds::`work: exits 0`, `kind+for`, `no-for`, `unknown kind` | T2-hermetic (argv-stub bd), 4 s | **KEEP as host** of `test-bead-file.sh`, T1; add missing `--repo`, unknown persona and label-less persona rows (G12) |
| 02 | test-bead-file-kinds::`event`, `escalation`, `proposal`, `insight`, `insight p2`, `event+repo`, `event no-repo`, `gate` | (shared) | KEEP (same host) |
| 03 | test-bead-lane-guard::`self-repo`, `dev-repo`, `override`, `builder plan` | T2-hermetic, 4 s | **MERGE-INTO test-bead-file-kinds.sh** (same fixture copy-pasted: fake fayth, repo-map, argv-stub bd, 14-var env) |
| 04 | test-bead-lint::all 10 cases | T3 (embedded Dolt, 3 resets, 4 seeds; every case runs `bead.sh lint` twice for rc and output), 20 s | **DEMOTE-TO-T1** 9 rows over canned `bd show --json`; keep 1 T2 real-bd row (`mixed --all`). Capture rc and output in one call. |
| 05 | test-bd-stdin::`no bare-dash bd note`, `no bare-dash bd create` + 3 controls | T0 run as a suite, 5 s (`grep -r` scans `chamber/` twice) | KEEP as **T0**: move to the lint stage and drop the double scan |
| 06 | test-gh-intake::19 cases | T2 (stub curl, and a stub bd that reimplements the store), 12 s | KEEP T2 for `trusted creates`, `idempotent rerun`, `closed beads still dedup`, `dry-run`, `unconfigured`. **SOURCE-GREP**: `no write constructed`, `no credential read`, `PRs excluded (static)` move to T0 (the PR check only greps for the word `pull_request`; replace with a behaviour row, since the stub already serves PR #999). Fix the vacuous `(e) comment absence is structural` ok(), and the `unconfigured` check that reads the logs of the previous run. Later, DEMOTE triage (a–d) to T1. |
| 07 | test-lanes::`roster split`, `roster non-empty`, `real roster`; test-fayth::`builder/ops in roster` | T1, 6 s + 2 s (shared) | MERGE both into the roster suite (**test-fayth.sh as host**) |
| 08 | test-fayth-predicates::`every summonable persona resolves`, `schema_name *`, `SPIRA_PLAN_LABEL=work`; test-fayth::`ops FAYTH_LABELS contains incident`; secondary test-spike::`spike.fayth fields` | T0+T1, 2 s | **MERGE-INTO test-fayth.sh** (runtime rows). **SOURCE-GREP/DELETE**: `builder FAYTH_LABELS built from $SPIRA_PLAN_LABEL`, `ops … $SPIRA_INCIDENT_LABEL`, `builder does not hardcode plan`, `ops does not hardcode incident` (exact source strings; the runtime override row already proves this). Keep the generic bare-literal scan as T0. |
| 09 | test-fayth::`summon_fayth ops asks its own predicate`, `ops predicate asked even when plan_ready=0`, `builder asks its own predicate`; test-spike::`partition (9 asserts)` | T1 2 s; spike T3 (shared) | KEEP T1 rows (drop the tautological `old code plan_ready=0` control). The spike partition rows stay T2 and are owned by safety-fences. |
| 10 | test-elastic-ceiling::all; test-lane-ceiling::(a)(b)(c) + controls; test-drain-expiry::`live drain gates`; test-lanes::`task pool=0 not summoned`, `lane without pool summoned` | T1, 2+1+2+(6) s | **MERGE** into one table-driven `test-summon-fayth.sh` (host: test-elastic-ceiling.sh; its counting `fayth_ready` stub is the template). Drop elastic `crit3` (verbatim duplicate of the first control). Add rows for halt and fleet-ceiling refusal (G5, G6); G7 (governor withhold) is stale — `spira/governor.sh` is gone. |
| 11 | test-fayth-free::all; test-lanes::`fayth_free arithmetic (4)` | T1, 4 s | **MERGE-INTO test-summon-fayth.sh**; delete the tautological `buggy formula disagrees` control, and replace it with a row that fails under `max-have` applied to a remainder |
| 12 | test-drain-expiry::all 7 | T1, 2 s | **MERGE-INTO test-summon-fayth.sh** (same stub fixture). This is the best seam test in the area: writer and reader are pinned in one file. |
| 13 | test-effective-lanes::crit1–6 | T1, 2 s | KEEP T1 (it can sit in the roster suite). Fix the vacuous `nowant 'effective:.*maechen-sweep'`, which is a glob literal and always passes. |
| 14 | test-sentinel-order::`pool=1/pool=3`, `CHECK7 before sending`, stamp rows; test-lane-ceiling::`(d) rotation` (**rotation reimplemented in the test**); secondary test-watchtower-throttle::`sentinel reads throttle stamp` (source grep); test-strand-truncated::case 4 (budget, real pass) | T3, 25 s | **KEEP test-sentinel-order.sh as the one T3 host** (rename `test-sentinel-pass.sh`), cut from 5 passes to 2 (fill+order in one pass, stamp-skip in a second). **SOURCE-GREP**: the rotation and throttle rows must go through an extracted `ck7_order`/`ck7_pool` in T1 (G1, G2). |
| 15 | test-lanes::`escape.sh summons`, `nothing ready`; `escape.sh contains 'control plane'` | T1 (shared) | KEEP both behaviour rows; **DELETE** the comment grep |
| 16 | test-unclaimable-bead::case1–12; test-unclaimable-cycle::control/guard/both | T3 47 s (12 resets); T1 72 s (anomalous: 3 python calls; declared timeout 60) | **DEMOTE-TO-T1** cases 1–7, 10, 12 plus the cycle rows as one fixture-JSON table in `test-unclaimable.sh` (host: test-unclaimable-cycle.sh, which already drives the real python body without a DB). Keep 1 T2 row (the bd-ready JSON shape) and the incident-filing rows 8, 9, 11 against a mock `incident.sh`. unclaimable-cycle is **SOURCE-GREP** in a different sense: it extracts code by splitting lib.sh text, so move the python body to `spira/unclaimable.py`. |
| 17 | test-cockpit-unclaimable::case1–4 (secondary) | T1-ish 3 s | KEEP (cockpit area); assert per-id attribution, since case 4 cannot tell which id got which label |
| 18 | test-check8-progressed::case1, case2 | T3 (Dolt + 2 full passes), 10 s | **DEMOTE-TO-T1** via a `check8_should_judge` predicate; add rows for cooldown and `plan_ready>0` (G15) |
| 19 | test-sentinel-capacity::all 6 | T3 (testdb + 2 passes), 6 s | **MERGE-INTO test-sentinel-order.sh**. The unreadable-DB pass needs no testdb (a bad path fails fast), and the readable control is the existing pass. |
| 20 | none (CHECK 2 reclaim loop, `bump_reclaim`, CHECK 2c `release_orphan_claims`) | — | **GAP** (G9, G10) |
| 21 | test-reclaim-needs-ryan::3; test-reclaim-escalated::cases 1–4; other area test-check2-reclaim | T1 2 s; T2 7 s | reclaim-needs-ryan **MERGE-INTO** the strand-classify table (test-strand-partition.sh, ops area). Keep reclaim-escalated as the single T2 chain and **DELETE** its case 1 (same control as needs-ryan). Recommend that test-check2-reclaim drop its label-only assertions. |
| 22 | test-unmapped-repo-park::all 5 | T3 5 s, **exercises only bd's own `--exclude-label`**, never aeon.sh | **DELETE (tests the dependency, not Spira)**; replace with a T1 test of the aeon.sh park path (G13) |
| 23 | test-fayth::`default CPUQuota=70%`, `custom CPUQuota=90%`; test-fayth-project-instructions (secondary) | T1 (shared); 2 s | MERGE the CPUQuota rows into test-summon-fayth.sh. project-instructions stays in aeon-execution, but covers only sweep mode (G17). |
| 24 | test-ops-allowlist (secondary) | T1 6 s | KEEP in safety-fences; the matcher is the test's own glob, not CLI semantics |

---

## 4. Duplicate clusters

| # | behaviour | files | evidence | keep |
|---|---|---|---|---|
| D1 | `bead.sh file` fixture and builder `--for` control | test-bead-file-kinds, test-bead-lane-guard | Mapper notes an identical copy-pasted 14-var env, fake fayth, repo-map and argv-stub bd; both run a builder `--for` control | **test-bead-file-kinds.sh**, renamed `test-bead-file.sh` |
| D2 | `summon_fayth` stub fixture (`aeon_count`, `capacity_paused`, `fayth_ready`, `SPIRA_SUMMON` mock) and the "held back" last-slot log | test-elastic-ceiling, test-lane-ceiling, test-drain-expiry, test-lanes, test-fayth | Five files each source lib.sh (5,571 lines) and rebuild the same stubs. `held back` is asserted in both ceiling suites. elastic `crit3` repeats its own first control verbatim. | one table **test-summon-fayth.sh** (from test-elastic-ceiling) |
| D3 | `fayth_free` elastic/pool arithmetic | test-fayth-free, test-lanes::`fayth_free arithmetic` | Same function and same elastic-returns-pool rows (0/2) | rows from test-fayth-free, inside D2's host |
| D4 | ops/builder predicate built from the incident/plan labels | test-fayth-predicates (exact source strings), test-fayth (runtime `fayth_get`), test-spike (`spike.fayth fields`) | The same property is checked three ways; the source-string form breaks on harmless refactors | runtime rows in **test-fayth.sh** plus one generic T0 literal scan |
| D5 | Roster discovery | test-fayth, test-lanes (`roster split`, `real roster`), test-spike (`fayth discovered without listing`), test-maechen (other area) | Each asserts that `spira_fayths` lists a chamber persona | **test-fayth.sh** |
| D6 | Unclaimable detector | test-unclaimable-bead (47 s real bd), test-unclaimable-cycle (hermetic), test-bead-lint::`open bead without partition` | Same python body in `detect_unclaimable_ready`; bead-lint's partition check is the file-time mirror | **test-unclaimable-cycle.sh** as the T1 table host, plus one T2 row |
| D7 | Ghost classifier control "plain dead worker → ghost" and skip-label exemption | test-reclaim-needs-ryan, test-reclaim-escalated case 1/3, test-check2-reclaim (label add/remove) | test-reclaim-escalated's header names the other two suites | classifier rows go to the strand-classify table. **test-reclaim-escalated.sh** keeps only the chain from the DB through JSON to the classifier. |
| D8 | Full sentinel pass on embedded Dolt with the same 9 stubs (pilgrimage, strand, governor, reflect, sending, mock-systemctl/launch/summon/notify) | test-sentinel-order (5 passes), test-sentinel-capacity (2), test-check8-progressed (2) | 9 passes and 3 testdb_up for about 5 distinct decisions | **test-sentinel-order.sh** as the single T3 pass suite (2 passes) |

---

## 5. Unit-extractable logic

| logic | where | tested today via | seam that enables T1 |
|---|---|---|---|
| Lint judgement over a bead's labels, status and type | `bead.sh` `_bead_lint` (the loop after `bdq show --json`) | Real Dolt, 20 s, two invocations per case | Split into `_bead_lint_judge <labels> <status> <type> <partitions>` (pure) and the read loop. Tests feed rows directly. |
| Unclaimable classification | `lib.sh detect_unclaimable_ready` embedded `python3 -c '…'` | Real bd (47 s), or string-splitting lib.sh (cycle) | Move the body to `spira/unclaimable.py` reading `PARTS`/`ALL_PARTS` and JSON on stdin (the same shape as `strand-classify.py`), then drive it from a fixture table |
| CHECK 7 lane rotation order | `sentinel.sh` l.1219–1232 | Reimplemented inside test-lane-ceiling (d), so the real code is never run | `lane_rotate <last> <lanes…>` in lib.sh |
| CHECK 7 task-pool computation (pool − task_live, throttle stamp → 0 unless override, per-persona fill cap) | `sentinel.sh` l.1199–1275 | Source grep (test-watchtower-throttle) and 5 full passes (test-sentinel-order) | `ck7_pool` and `ck7_fill <f> <pool>` in lib.sh; the sentinel loop becomes a call |
| CHECK 8 predicate and cooldown | `sentinel.sh` l.1379–1400 | Dolt + 2 passes (10 s) | `check8_should_judge plan_ready plan_inprog n_open progressed last now every` → yes / cooldown / no |
| Reclaim output parsing (`✓`/`Reclaimed` lines → ids → `bump_reclaim`) | `sentinel.sh` l.185–194 | Untested | `parse_reclaimed <bd-output>`; stub `bdq reclaim` |
| DB-unreadable probe | top of `sentinel.sh` (`bdq` probe) | Testdb + 2 passes | A failing `SPIRA_BD` shim; no Dolt |
| Lane admission for `bead.sh file` | `bead.sh` l.66–98 (vocab scan + `spira_repo_lanes`) | Already hermetic (stub bd) | Factor out `bead_lane_admits <persona-labels> <repo>` so the table needs no `bead.sh` process |
| GitHub triage/dedup | `gh-intake.sh` `_accept_actor`, `_ingested`, `_create_*` | ~15 full script runs with python3 per run | Source the script's functions under a `GH_INTAKE_LIB=1` guard and call them over issue JSON |
| Unmapped-repo park | `aeon.sh` (ask+overseer label before `release_own_claim`) | Not tested (the suite tests bd only) | Extract `park_unmapped <id>` into lib.sh; stub bd to record argv order |
| Launch argv (CPUQuota, TimeoutStartSec, `--setting-sources`) | `summon_fayth` l.1440, `escape.sh`, `aeon.sh` ×2 sites | Full aeon.sh sweep with an agent shim | `aeon_claude_argv <sys-flag> <sys-file>` (landed independently by sp-eq8a4.2.5) and `summon_argv <fayth>` (sp-9ce60.4), shared by summon and escape |

---

## 6. Gaps

1. **G1: lane rotation is never exercised.** test-lane-ceiling::(d) says it "mirrors sentinel.sh logic" and runs its own copy. A broken `sentinel.sh` l.1219–1232 would pass. (sp-cm29p)
2. **G2: throttle → task pool 0 is tested only by source grep.** test-watchtower-throttle l.308–318 greps `SPIRA_THROTTLE_STAMP` in sentinel.sh. Nothing tests the behaviour that lanes stay unaffected (sp-h7zzx) or that `SPIRA_QUEUE_THROTTLE_OVERRIDE=off`.
3. **G3: per-persona fill cap is untested.** The fill-loop break `_fill >= ${SPIRA_MAX_LIVE_AEONS:-4}` with no pool set (sentinel.sh l.1272) is what stops an unbounded summon loop on a host without a pool.
4. **G4: `FAYTH_RESERVE` is documented but not implemented.** sentinel.sh l.1188 says "Ops also RESERVES a slot (FAYTH_RESERVE in its .fayth)". `grep FAYTH_RESERVE lib.sh chamber/*.fayth` finds nothing. Either the spec is dead or a feature was lost; decide, then test or delete the comment.
5. **G5: RESOLVED (sp-9ce60.2.1).** A direct row in test-summon-fayth.sh now asserts that `summon_fayth` returns 1 and logs `halted — not summoning` while `world.halted` is present; previously asserted only by test-world.sh's source grep.
6. **G6: RESOLVED (sp-9ce60.2.1).** A direct row in test-summon-fayth.sh now asserts `N/M aeon(s) live across the whole fleet — not summoning` (lib.sh) at the plain fleet ceiling, distinct from the last-slot rules the ceiling suites already covered.
7. **G7: STALE, noted MOOT (sp-9ce60.2.1).** `spira/governor.sh` and every `SP_GOVERNOR*`/governor reference are confirmed gone from `lib.sh` as of sp-8mzsh landing on `main`. The `enforce`-mode clamp this gap named no longer exists in `fayth_free`, and UC-dispatch-11 above has been trimmed of its governor-clamp clause to match (sp-9ce60.4). No governor-mode row is written for `fayth_free` or anywhere else in this area.
8. **G8: PARTIALLY RESOLVED.** `escape.sh` hard-coded `CPUQuota=70%`, ignoring `SPIRA_AEON_CPU_QUOTA`; fixed (sp-9ce60.4) by sharing `summon_argv()` (lib.sh) with `summon_fayth`, so the flag now reaches `escape.sh` exactly as it reaches `summon_fayth`. The halt/drain question is unchanged: sp-9ce60.2.1 wrote a characterization row (test-summon-fayth.sh) asserting today's bypass and opened the decision as sp-6rv05, still unanswered. An mail asking the same question was sent independently (sp-9ce60.4) before sp-6rv05 was found; sp-6rv05 is canonical. `escape.sh` still does not call `world_gate()` — only `summon_fayth` does — pending that decision.
9. **G9: the CHECK 2 stale-lease reclaim is untested.** The loop over `fayth_partitions`, the `--exclude-label $SPIRA_RECLAIM_SKIP_LABEL` flag, the `bump_reclaim … stale-lease` charge and the `no persona … declares a partition` log (sentinel.sh l.179–196) have no test. Only the classifier half (CHECK 2b) is covered.
10. **G10: CHECK 2c `release_orphan_claims` has no test and hard-codes the plan partition.** It is called with `${SPIRA_SCOPE_LABEL},plan` (sentinel.sh l.264), and the recount uses a literal `spira-poison`. This is the "one hardcoded partition" defect that the `fayth_partitions` comment (lib.sh l.1238) says was removed from CHECK 2 and CHECK 5. Orphaned claims in ops, spike or groom partitions are never released.
11. **G11: two implementations of the bead contract, and the harness one accepts unmapped repos.** Harness `spira/bead.sh file --for builder --repo typo` succeeds, because `spira_repo_lanes` on an unmapped repo returns every lane (lib.sh l.4035–4043), so the bead gets `repo:typo`. CHECK 5 later skips it ("not in repo-map"). The brain's `.claude/bead.sh` (41 KB) refuses unmapped repos and re-checks claimers, but it is tested only by `brain/.claude/test-bead.sh`, outside harness CI. The CLAUDE.md claim "requires a `repo:` the repo-map resolves" holds only for the brain copy.
12. **G12: several `bead.sh file` refusals are untested.** `--repo <name> required`, `no such persona`, and `persona … has no partition labels` (bead.sh l.59–64) have no rows.
13. **G13: the unmapped-repo park path is untested** (sp-4l0d, sp-nlhy, sp-foi7). test-unmapped-repo-park adds labels by hand and never calls aeon.sh or lib.sh. Its header claims `covers: aeon.sh lib.sh`.
14. **G14: `bead.sh contract` output is untested in the harness.** It is exercised only through test-concierge.
15. **G15: two CHECK 8 conditions are untested.** Nothing covers the cooldown (`inference is in cooldown`) or the rule that `plan_ready>0` exits before judgement even when an incident is ready (sentinel.sh l.1373–1395).
16. **G16: RESOLVED (sp-9ce60.2.2).** A direct row in test-fayth.sh now checks `fayth_exclude`'s exclusion of other personas' `fayth:` labels and of `SPIRA_QUEUE_WAIT_LABEL` in the predicate (lib.sh l.914), previously exercised only indirectly through the detectors.
17. **G17: RESOLVED, independently (sp-eq8a4.2.5).** aeon.sh's two launch sites (sweep, bead) already call one shared `aeon_claude_argv <sys-flag> <sys-file>` (lib.sh) by the time this bead reached it — a different branch extracted the identical seam this gap asked for. The function takes no sweep/bead mode argument at all: it is a pure function of the fayth's own knobs, and test-aeon-prompt-layers.sh's table drives it directly (both system-prompt-flag shapes) proving `--setting-sources` is independent of which launch site calls it. sp-9ce60.4 built its own `aeon_argv <fayth> <mode>` before discovering this and dropped it on rebase rather than duplicate the seam.
18. **G18 (test-integrity): three tests pass for the wrong reason or prove nothing.**
    - test-id-prefix::`guard allows when custom-prefix id present` passes because the guard fails open when `bd show` cannot reach a db.
    - test-ops-allowlist uses a local glob matcher, not the CLI's permission semantics.
    - test-fayth-free and test-fayth "positive controls" assert constants or the stub's own return.

---

## 7. Cost

**Now.** Summing ci_secs over the 21 primary files:
5 (bd-stdin) + 4 (bead-file-kinds) + 4 (bead-lane-guard) + 20 (bead-lint) + 10 (check8) + 2 (drain-expiry) + 2 (effective-lanes) + 2 (elastic-ceiling) + 4 (fayth-free) + 2 (fayth-predicates) + 2 (fayth) + 12 (gh-intake) + 1 (lane-ceiling) + 6 (lanes) + 7 (reclaim-escalated) + 2 (reclaim-needs-ryan) + 6 (sentinel-capacity) + 25 (sentinel-order) + 47 (unclaimable-bead) + 72 (unclaimable-cycle) + 5 (unmapped-repo-park) = **240 s**.
Of that, 72 s is test-unclaimable-cycle, a 60-line file that runs 3 python processes and no DB. The number is a measurement anomaly (runner contention or queueing) rather than a real cost. Without it the baseline is **168 s**.

**Projected after the verdicts** (21 files → 9 suites + T0 lint rows):

| suite (tier) | absorbs | projected s | arithmetic |
|---|---|---|---|
| T0 lint rows (bare-dash, bare-literal FAYTH_LABELS, gh-intake token greps) | bd-stdin, parts of fayth-predicates and gh-intake | 2 | one scan of spira/ and chamber/ once, instead of 5 s with a double scan |
| test-bead-file.sh (T1) | bead-file-kinds + bead-lane-guard + G12 rows | 4 | 8 → 4: one fixture, one conf.sh source per row |
| test-bead-lint.sh (T1 + 1 T2 row) | bead-lint | 6 | 9 rows T1 ≈ 1 s; 1 testdb_up + seed ≈ 5 s (was 3 resets, 4 seeds, 2× invocations = 20 s) |
| test-fayth.sh (T1 roster/predicates) | fayth + fayth-predicates runtime + lanes roster + effective-lanes | 3 | 2+2+2(lanes share)+2 = 8 → 3: one lib.sh source |
| test-summon-fayth.sh (T1 table) | elastic-ceiling + lane-ceiling + drain-expiry + fayth-free + lanes (summon/free/escape) + fayth CPUQuota + G5–G8 rows | 4 | 2+1+2+4+4(lanes rest) = 13 → 4: one lib.sh source instead of six |
| check8 predicate (T1) | check8-progressed + G15 rows | 1 | 10 → 1: no Dolt, no pass |
| test-gh-intake.sh (T2) | gh-intake | 8 | 12 − static rows − duplicate reruns ≈ 8 |
| test-reclaim-escalated.sh (T2 chain) + classifier rows | reclaim-escalated − case 1; reclaim-needs-ryan → strand table | 7 | 6 + 1 (the 3 rows added to the strand-classify table, costed here) |
| test-sentinel-pass.sh (T3, batch/main) | sentinel-order + sentinel-capacity (+ G1–G3 through extracted functions in T1) | 12 | 25 + 6 = 31 (7 passes, 2 testdb_up) → 2 passes on one testdb ≈ 10 s + unreadable-DB pass with no testdb ≈ 2 s |
| test-unclaimable.sh (T1 table + 1 T2 row) | unclaimable-bead + unclaimable-cycle | 8 | 47 + 72 = 119 → T1 ≈ 2 s + one testdb_up/seed/`bd ready` ≈ 6 s |
| park_unmapped (T1) | replaces unmapped-repo-park | 1 | 5 → 1 |
| new gap rows G9/G10 (T1 parse + stub) | — | 1 | new |
| **total** | | **57 s** | 2+4+6+3+4+1+8+7+12+8+1+1 |

**240 s → 57 s** (−183 s, −76%). Against the anomaly-free baseline, **168 s → 57 s** (−66%). This is after adding the gap coverage from G1–G3, G5–G10, G12, G13 and G15.

Placement after the change:
- T0 and T1 (about 30 s) run in certification on every commit.
- T2 (bead-lint row, gh-intake, reclaim chain, unclaimable row; about 19 s) runs in batch CI.
- T3 test-sentinel-pass (12 s) runs in batch CI and on main.

Nothing in this area needs T4. Acceptance already runs real sentinel passes on the installed tarball.

---

## 9. Implementation status (sp-9ce60, landed)

The five dependencies this bead forked ahead of (`spira/testlib.sh`/sp-yivi7,
sp-qvjzb, sp-qu948, sp-8mzsh, sp-pnhtt) are all reachable from `origin/main` now.
Everything the first slice deferred pending them has since landed, across this bead's
own children (sp-9ce60.1 through .8) and this closing session's rebase onto `origin/main`:

- This page, `docs/test-plan/dispatch.md`, verbatim per the approved design, with the
  `spira/plan-lint.sh`-readable `UC-dispatch-NN` declarations in §2, and every suite below
  tagged `# tier:`/`# covers: UC-dispatch-NN`.
- **D1–D8 applied.** D1: `test-bead-file-kinds.sh` + `test-bead-lane-guard.sh` →
  `test-bead-file.sh` (adds the G12 refusal rows and UC-dispatch-03's unmapped-repo
  refusal, sp-pnhtt). D2/D3/D4/D5: `test-elastic-ceiling.sh`, `test-lane-ceiling.sh`,
  `test-drain-expiry.sh`, `test-fayth-free.sh`, `test-lanes.sh` and
  `test-fayth-predicates.sh` → `test-summon-fayth.sh` (arithmetic) and `test-fayth.sh`
  (roster/predicates), one `lib.sh` source and one stub set instead of six. D6:
  `test-unclaimable-bead.sh` + `test-unclaimable-cycle.sh` → `test-unclaimable.sh`, the
  classifier body moved to `spira/unclaimable.py`. D7: `test-reclaim-needs-ryan.sh` merged
  into `test-strand-partition.sh`'s classifier table and `test-reclaim-escalated.sh`'s T2
  chain. D8: `test-sentinel-order.sh` + `test-sentinel-capacity.sh` + the two full-pass
  cases from `test-check8-progressed.sh` → `test-sentinel-pass.sh` (T3, 2 Dolt passes
  instead of 9).
- **Row 05/06 verdicts applied** via dedicated fences rather than one combined grep
  script: `test-bd-stdin.sh` now tests `spira/bd-stdin-lint.sh` (T0, wired into
  `spira/gate-fences.sh`); `gh-intake.sh` gained the `GH_INTAKE_LIB=1` seam so
  `test-gh-intake.sh` drives `_accept_actor`/`_ingested`/`_create_work` directly, and its
  static rows moved to `spira/gh-intake-lint.sh`.
- **G1–G3, G9, G10, G13–G18 closed.** `lane_rotate`, `ck7_pool`, `ck7_fill_cap`,
  `check8_should_judge`, `check2_reclaim_stale` and `parse_reclaimed` are `lib.sh` seams
  with direct T1 tables (`test-summon-fayth.sh`, `test-watchtower-throttle.sh`,
  `test-check8-progressed.sh`, `test-check2-reaper.sh`). G4 (`FAYTH_RESERVE`): the dead
  comment describing an unimplemented reservation was removed; there is no feature to
  test. G5/G6: the halt gate and the plain fleet-ceiling refusal each have a positive
  control plus a refusal row in `test-summon-fayth.sh`. G7 (governor withholding): moot,
  `spira/governor.sh` is deleted (sp-8mzsh) and `test-summon-fayth.sh` asserts its absence.
  G8 (escape.sh vs. halt/drain): characterized as current behaviour in
  `test-summon-fayth.sh`, filed as a decision bead (sp-6rv05) rather than changed here; the
  `CPUQuota` half of G8 (hard-coded 70%) is fixed and tested. G11: `bead.sh file` now
  refuses an unmapped `--repo` before calling `bd create` (sp-pnhtt), tested in
  `test-bead-file.sh`. G13: `spira/test-unmapped-repo-park.sh` deleted (it exercised only
  `bd`'s own `--exclude-label`); `park_unmapped` is extracted and tested directly. G14:
  `bead.sh contract` output has its own suite (`test-bead-contract.sh`). G16:
  `fayth_exclude` has a direct table in `test-fayth.sh`. G17/G18: bead-mode
  `--setting-sources` and the acted/progressed distinction each have a direct row.
- **C1 applied**: `spira/governor.sh` is gone from `main`; no governor-related test rows
  remain to delete.
- **UC-dispatch-22 verdict applied**: `spira/test-unmapped-repo-park.sh` deleted (G13
  already named it as exercising only `bd`'s own `--exclude-label`, never `aeon.sh`).

Not applicable: the "implement verdict V7 via C2 (depend on it; do not duplicate it)"
instruction was raised to the operator (mail `sp-38xyc`, 2026-09-24) — the dispatch page's
own numbering has no "V7" or "C2", those ids belong to the epic's section 9 master list
this bead cannot read. No reply arrived; per that mail's stated default, treated as not
applicable to this area (a likely copy-paste from another area's instructions) and worked
accordingly across every child bead without further escalation.

Deferred, as follow-up work under this bead's epic (`sp-s088v`), not blocking this close:

- G6's cooldown/`plan_ready` combination and any UC this page marks T2/T3-only (UC-06
  trusted-credential rows, UC-16's real-`bd`-shape row) stay at their assigned tier; this
  bead did not lower tiers the plan did not ask for.
