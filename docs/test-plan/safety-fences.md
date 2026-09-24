# Test plan — Safety fences and guards (`safety-fences`)

Part of [[test-plan-2026-09-23]], section 5. Area id `safety-fences`; use-case ids are `UC-safety-fences-NN`.

Primary files (22): test-aeon-dirty-commit, test-aeon-fence-bd-create, test-aeon-fence, test-aeon-worktree-guard, test-bd-close-unacked-guard, test-bd-delivers-label-guard, test-bd-remember-guard, test-bd-update-inflight-guard, test-branch-guard, test-destructive-bead, test-focus-close-guard, test-host-reason, test-inventory, test-mail-fence, test-null-close-reason, test-ops-allowlist, test-orphan-test, test-ref-guard, test-scratch-fence, test-set-state-writers, test-spike, test-testenv-batch-fence.
Secondary (5): test-czar-shadow, test-freshclone, test-schema-migration-guard, test-suite-state-fence, test-suite-state.
Sources: map/*.jsonl records for these 27 files, reduce/r0–r3 (agent-guardrails, agent-safety-fences, fences-and-guards), signals.tsv (main-push run 35947142904), and read-only reads of `spira/aeon.sh`, `spira/hooks/*`, `spira/worktree-hooks.sh`, `spira/gate-spira.sh`, `brain/.claude/settings.json`.

Dimension codes used below: **C** correctness, **FC** fail-closed, **O** observability (refusal names cause, remedy, override), **I** idempotency, **CFG** config-compat, **K** contract, **TI** test-integrity.
Where it runs: **cert** = certification/gate plus local (T0/T1), **batch** = batch CI (T2), **main** = main-push CI (T3).

---

## 1. Intent

A fence is a Rung-4 mechanism: a program that refuses an honest mistake at the moment it is made and names its own override. The tests pin down four families.

1. **PreToolUse hooks on agent sessions.** They read Claude Code's hook JSON on stdin and refuse specific shell or tool actions: queue and landing operations, writes into production, mail and landstate, test-data bead creation, closing without an ACK or a cited bead, statute writes, spec edits on in-flight beads, `bd update --status closed`, direct suite execution and `DELETE FROM schema_migrations`. They let through non-aeon sessions (or, for the inflight guard, aeon sessions), non-Bash tools, quoted prose and heredoc bodies, sanctioned wrapper scripts, and any command carrying the named override.
2. **Git hooks.** `pre-commit` (made up of `exclude.sh`, `scratch-fence.sh` and `branch-guard.sh staged`), the per-worktree `pre-commit-guard.sh` dirty-path guard, and the `reference-transaction` guard on `refs/heads/spira/*`. Together they keep an aeon's commits inside its own worktree and branch, and keep its deletes away from other aeons' branches.
3. **Shipped-tree and gate-time lints.** `inventory.sh`, `scratch-fence.sh`, `orphan-test.sh`, `host-check.sh`, `suite-state-fence.sh`, the set-state writer lint, the chamber placeholder/command/reopen lints and the syntax sweep. They refuse a tree that should not ship, and they exit 3 or `?` rather than report "clean" on an empty input.
4. **Persona scoping.** `FAYTH_TOOLS` allowlists, the czar stage fence, spike `confine.sh`, and `bdq create`'s destructive-vocabulary refusal. Each limits what a persona or bead can do, even when the agent asks for more.

Every refusal must name the rule, the correct path and the override. Every fence must be armed where the offender actually runs (`law-guard-binds-the-caller`, `law-guard-proved-where-the-offender-runs`).

---

## 2. Use cases

| ID | Requirement | Dims | Tier | Runs |
|---|---|---|---|---|
| UC-safety-fences-01 | In an aeon session, queue- and landing-operating commands (`batch.sh`, `git push`, `gh pr create`, `queue.sh flush`, `verdict.sh`, `landing.sh`) are refused. `queue.sh stats` is allowed. A blocked script's name used as a git file argument or as a suite name is not treated as an invocation, but a compound that also invokes it is. | C, O | T1 | cert |
| UC-safety-fences-02 | In an aeon session, writes under `SPIRA_PROD` or `SPIRA_RUN/landstate` are refused: redirect, rm, mv, tee, `sed -i`, and `git -C` add/commit/reset/checkout/clean. Reads and exec from prod, rendered brief commands (sop/incident/mail/suites/groomer), `census.sh`, and prod paths in heredoc/prose are allowed. | C, FC | T1 | cert |
| UC-safety-fences-03 | In an aeon session, `bd create` against the production store is refused for a test-data title or a `repo:` label missing from the repo map. An inline `SPIRA_DB=` does not get around it. A non-production path passes, and `SPIRA_BD_CREATE_OVERRIDE=1` bypasses. | C, FC, CFG | T1 (+1 T2 contract row for bd's `--dry-run` "appears to be test data" text) | cert / batch |
| UC-safety-fences-04 | An aeon may not `bd close` while an operator comment posted after its claim is unacknowledged. An `ACK <uuid>` note satisfies the guard. Pre-claim comments and the aeon's own comments are exempt. The refusal names the comment id and `SPIRA_CLOSE_UNACKED_CONSIDERED`. | C, O, K | T1 (canned `bd show/comments --json`) | cert |
| UC-safety-fences-05 | A post-claim operator comment is delivered to the aeon exactly once as PostToolUse `additionalContext`, naming the ACK command. Pre-claim and self comments are not delivered. | C, I | T1 | cert |
| UC-safety-fences-06 | An aeon may not remove any `delivers:*` label. The refusal names the criterion's producer. `incident.sh retire-unsatisfiable-delivers` and ordinary labels are allowed. | C, O | T1 | cert |
| UC-safety-fences-07 | An aeon may not write `sop-*` / `law-*` keys with `bd remember` (`--key`, positional, `-C` forms). The refusal names `sop.sh write` / `rule.sh enact`, and those sanctioned writers pass. | C, O | T1 | cert |
| UC-safety-fences-08 | A non-aeon session may not edit spec fields (description/design/acceptance/body-file/design-file/-d) of an in-flight bead (in_progress + assignee + branch label). Non-spec flags, not-in-flight beads and aeon sessions pass. The refusal suggests `bd comment`. | C, O, K | T1 (canned `bd show`) | cert |
| UC-safety-fences-09 | During a focus period (narrowed `SPIRA_FAYTHS`), an aeon's `bd close` must cite an open bead or a closed insight bead, in either `--reason` or `--reason-file`. With the full roster the guard is a no-op. | C, CFG | T1 (stub `bd show` for 3 states) | cert |
| UC-safety-fences-10 | In an aeon session, `bd update --status closed` in any argv form is refused with exit 2, naming `bd close`. `bd close` and other status values pass. | C, O | T1 | cert |
| UC-safety-fences-11 | In an aeon session, writes into `SPIRA_MAIL` are refused, whether by Bash redirect/mv/cp/touch or by the Write/Edit tools, for both the literal and the expanded path. `mail.sh send` and reads are allowed. | C, O | T1 | cert |
| UC-safety-fences-12 | In an aeon session, direct execution of `spira/test-*.sh` in any shell shape is refused, naming `testenv-batch.sh`. `testenv-batch.sh` runs and reading a suite are allowed. | C, O | T1 | cert |
| UC-safety-fences-13 | `DELETE FROM schema_migrations` is refused as a tool call (`bd sql` / `dolt sql`, case- and whitespace-insensitive). It is also refused at `bdq create` even when the bead carries needs-ryan. Both refusals point at `bd migrate schema`. | C, FC | T1 | cert |
| UC-safety-fences-14 | `bdq create` refuses harness-halting vocabulary (world stop, `systemd/install.sh`, daemon-reload, `systemctl stop/restart spira-*`, schema migrate) unless the bead carries the ask label. The refusal happens before `bd` is invoked, and near-miss text passes. | C, FC, CFG | T1 | cert |
| UC-safety-fences-15 | **Shared PreToolUse contract:** every guard blocks through one documented protocol and names guard, remedy and override. The override works from env and inline. The guard is inert outside its session scope, for tools it does not own, and for single-quoted/heredoc prose. Behaviour on an empty or malformed payload is defined and tested. | K, FC, O, TI | T1 (one table) | cert |
| UC-safety-fences-16 | **Aeon sessions actually carry the fences:** `aeon_settings` registers every guard meant for aeons, identically for the sweep and the claimed launch. A guard that is missing or not executable is reported, never silently dropped. | FC, K, O | T1 | cert |
| UC-safety-fences-17 | An aeon commit that stages a path already dirty before the session began is refused, naming the path. Own-path commits succeed. `SPIRA_ALLOW_DIRTY_STAGE=1` overrides. | C, O | T2 (tmp git) | batch |
| UC-safety-fences-18 | `branch-guard.sh staged` refuses an aeon-identity commit on the base branch, and an aeon commit outside `SPIRA_WORK`, naming email/branch/both paths. Operator identity, non-base branches and sweeps (no `SPIRA_WORK`) pass. | C, O, CFG | T2 | batch |
| UC-safety-fences-19 | `branch-guard.sh check` reports an aeon commit at a base tip and a shared checkout ahead of its remote. It reports clean after an operator commit on top. | C, O | T2 | batch |
| UC-safety-fences-20 | The `reference-transaction` hook refuses unsanctioned deletion of `refs/heads/spira/*` (including `spira/queue/*`), including from inside an aeon worktree. `SPIRA_REF_SANCTIONED=1` allows it, non-spira refs are untouched, the committed phase exits 0, and large stdin is drained without SIGPIPE. | C, FC | T2 | batch |
| UC-safety-fences-21 | `worktree-hooks.sh install` arms every canonical hook in an aeon worktree. Its composed `pre-commit` runs the canonical fences first and then the dirty-path guard. An install failure in `aeon.sh` is loud. | FC, K | T2 | batch |
| UC-safety-fences-22 | The tracked `hooks/pre-commit` runs `exclude.sh staged`, `scratch-fence.sh` and `branch-guard.sh staged` in a clone armed by `exclude.sh install`, and each one can refuse through the hook. | K | T2 | batch |
| UC-safety-fences-23 | `exclude.sh` keeps beads data (`.beads/`, `.dolt/`, JSONL exports) out of the harness tree and out of staged files. | C, FC | T1 (path list) + T2 (staged) | cert / batch |
| UC-safety-fences-24 | `scratch-fence.sh` refuses tracked root `sp-*` / `*.fixed` files, ignores subdirectories, exits 3 on an empty index, and honours `SCRATCH_FENCE_OK`. | C, FC | T1 (file-list seam) | cert |
| UC-safety-fences-25 | `inventory.sh` refuses personal paths (`/home/<u>/`, `/workspaces/`), `(per Name, date)` marks and real emails in tracked and untracked files. It exempts its own pattern files and example domains, exits 3 on an empty index, and supports `--scan` / `--patterns`. | C, FC | T1 (`--scan` table) + T2 (tree walk) | cert |
| UC-safety-fences-26 | `orphan-test.sh` refuses a diff that removes a token an unmodified suite still asserts on. It passes when the suite changes in the same diff, on an `orphan-test-ok` annotation, and on a token present on both `-` and `+` lines. It exits 77 without `SPIRA_GATE_BASE`. | C, FC | T2 | batch |
| UC-safety-fences-27 | `host-check.sh` refuses a host suite that has neither a non-empty `# host-reason` nor testenv use in code. Its counts render a number or `?`, never 0 on an empty tree. | C, FC, O | T1 (fixture dir) | cert |
| UC-safety-fences-28 | `suite-state-fence.sh` refuses a quarantine entry with no bead, a CLOSED bead, a nonexistent suite or no reason. An empty file reports clean. | C, FC | T1 (stub bd status) | cert |
| UC-safety-fences-29 | No harness script writes a dimension label (repo/severity/branch/fayth/lane/gate) with `bd label add` or embeds one in `SPIRA_INCIDENT_LABELS`; `bd set-state` is the only writer. | K | T0 lint (+ bd set-state semantics in the bd-contract T2) | cert / batch |
| UC-safety-fences-30 | Persona tool allowlists: no restricted persona (Ops, czar) gets bare `Bash`. Each allows its diagnostics and denies mutations (rm, `systemctl start/stop/restart`, `git branch -D`, destructive bd). | C, CFG, TI | T0/T1 | cert |
| UC-safety-fences-31 | The czar stage fence: each class defaults to shadow and refuses, naming `SPIRA_CZAR_STAGE_<CLASS>`. `act` allows. `queue.sh eject` honours the fence before touching the DB. | C, FC, CFG | T1 | cert |
| UC-safety-fences-32 | Spike confinement: `confine.sh` refuses a spike branch touching files outside `SPIRA_SPIKE_PATHS`, matching by path prefix against the merge base and binding to the spike persona. An unreadable bead fails open (documented). `landing.sh` reopens an unconfined spike without merging it and keeps the branch. | C, FC, CFG | T2 (confine) + T3 (landing) | batch / main |
| UC-safety-fences-33 | Chamber integrity: every `{{PLACEHOLDER}}` is rendered by its program, every command a brief names exists, every fayth parses, spike/ops brief clauses are present, and `bd reopen` is called only through `bead_reopen`. | K | T0 | cert |
| UC-safety-fences-34 | Shipped-tree readiness: every shipped shell script parses, `conf.sh` resolves `SPIRA_HOME` without operator config, and the ask-label default names no operator. | K, CFG | T0 | cert |

---

## 3. Coverage map

Costs are ci_secs from main-push run 35947142904. "T?" gives the current level according to the mapper.

| UC | Existing tests (file::case) | Now / cost | Verdict |
|---|---|---|---|
| 01 | test-aeon-fence::A1–A5, E0–E3, I0–I4 | T1-ish, 10 s (shared with 02) | MERGE-INTO test-guards.sh (row table) |
| 01 (static half) | test-aeon-fence::D0–D3 (grep archivist.md for prohibitions) | source grep | SOURCE-GREP → move to the UC-33 chamber lint (T0) or delete; the prose is not the fence |
| 02 | test-aeon-fence::A6, A7, F0–F16, G0, G(8 brief cmds), H0/H1 | T1, (in 10 s) | MERGE-INTO test-guards.sh |
| 03 | test-aeon-fence-bd-create::positive control, A1, A2, A3, B1–B3, C1, D1 | T2 (real bd + testdb_up), 6 s | DEMOTE-TO-T1 + MERGE-INTO test-guards.sh. Drop A2 and the control (identical command to A1, run 3×). One row goes to test-bd-contract.sh |
| 04 | test-bd-close-unacked-guard::guard blocks…, names id, names override, exits 2, ACK note, env/inline override, pre-claim, self-comment, fails-open baseline | T3-ish (real Dolt, 6× `sleep 2`), 39 s | DEMOTE-TO-T1 (stub bd with canned timestamps; removes 12 s of sleeps and the double `run_guard`) |
| 05 | test-bd-close-unacked-guard::deliver outputs additionalContext…, second deliver empty, pre-claim/self not delivered | (in 39 s) | DEMOTE-TO-T1, kept as its own rows (PostToolUse, not PreToolUse) |
| 06 | test-bd-delivers-label-guard::PC, A1–A3, B1–B2, D1 | T2 (testdb), 6 s | DEMOTE-TO-T1 + MERGE-INTO test-guards.sh. DELETE "label remains after blocked remove" (vacuous: the guard never runs the command) |
| 07 | test-bd-remember-guard::all | T1, 9 s | MERGE-INTO test-guards.sh |
| 08 | test-bd-update-inflight-guard::all | T2 (testdb), 9 s | DEMOTE-TO-T1 + MERGE-INTO test-guards.sh |
| 09 | test-focus-close-guard::all | T2 (testdb), 5 s | DEMOTE-TO-T1 + MERGE-INTO test-guards.sh. Pin the chamber fixture rather than exit 77 when <2 auto personas |
| 10 | test-null-close-reason::all | T1, 2 s | MERGE-INTO test-guards.sh (rename: the file is named after the defect, not the guard) |
| 11 | test-mail-fence::all | T1, 4 s | MERGE-INTO test-guards.sh (add exit-code column) |
| 12 | test-testenv-batch-fence::all | T1, 3 s | MERGE-INTO test-guards.sh (add exit-code column) |
| 13 | test-schema-migration-guard::all (secondary); test-destructive-bead::schema-delete × 6 | T1, 3 s + (in 4 s) | MERGE schema-migration-guard INTO test-guards.sh. Keep the bdq rows in test-destructive-bead (a different mechanism) and share the input strings. Fix the vacuous `want(...,'')` / unconditional `ok` rows |
| 14 | test-destructive-bead::sp-6ylz … bdq e2e | T1, 4 s | KEEP (exemplar unit suite) |
| 15 | pass-through rows copied into all 10 guard suites (non-aeon, non-Bash, `'prose'`, heredoc, env override, inline override) | ~60 rows across 93 s of suites | MERGE into one parameterised block in test-guards.sh that runs against every guard. Add a malformed-payload row and a uniform exit-protocol column |
| 16 | none | — | GAP → new T1 rows in test-guards.sh (call `aeon_settings` with a tmp `SPIRA_HOME`) |
| 17 | test-aeon-dirty-commit::all | T2 inside podman; NA in CI (never ran) | KEEP, but fix the harness: drop the podman re-exec, use `GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1`, and drive the *composed* worktree hook (UC-21) instead of a hand-written one |
| 18 | test-branch-guard::aeon on base…/operator/non-base; test-aeon-worktree-guard::all | T2, 4 s + 2 s | MERGE test-aeon-worktree-guard INTO test-branch-guard (same script, same git fixture) |
| 19 | test-branch-guard::check… | T2, (in 4 s) | KEEP |
| 20 | test-ref-guard::cases 1–9 | T2, 1 s | KEEP (right level, strong controls) |
| 21 | test-ref-guard::worktree keeps pre-commit (existence only) | partial | KEEP + extend: assert the composed hook runs exclude/scratch/branch-guard *and* pre-commit-guard |
| 22 | test-scratch-fence::pre-commit hook calls scratch-fence (grep); test-branch-guard copies hooks/pre-commit but never runs it | source grep | SOURCE-GREP → one behaviour row: commit through `hooks/pre-commit` in an armed tmp clone, each fence seen to refuse |
| 23 | none (exclude.sh, 336 lines, is gate fence #1 and pre-commit fence #1) | — | GAP |
| 24 | test-scratch-fence::empty index … sp-* in subdirectory | T2, 3 s | KEEP. Replace the gate-spira grep row with the UC-22 / gate-fence-list behaviour row |
| 25 | test-inventory::empty index … --patterns | T2, 3 s | KEEP. DELETE "every shipped file passes inventory" (re-runs the gate's own fence). SOURCE-GREP "gate-spira.sh names inventory.sh" → covered by the gate-fence-list row |
| 26 | test-orphan-test::all | T2, 12 s | KEEP. DELETE "broken matcher does not refuse" and "real fence still refuses after restoration" (they swap in a stub script, a tautological control) |
| 27 | test-host-reason::all except suites.sh status; test-host-reason::suites.sh status rows | T2, 14 s | KEEP host-check rows (demote the git fixture to a plain dir if host-check reads files, not the index). MOVE the two `suites.sh status` rows (the real shipped-tree sweep, most of the 14 s) to test-infrastructure, and make them unconditional |
| 28 | test-suite-state-fence::all (secondary); test-suite-state::lint rejections | T2 4 s; T1 2 s | MERGE the structural rows of test-suite-state-fence INTO test-suite-state (already there). Keep one CLOSED-bead row with stub bd (DEMOTE-TO-T1). Add a pass/fail summary line |
| 29 | test-set-state-writers::scanner controls + 2 static rows; ::set-state semantics rows | T2 (testdb), 8 s | Split: lint rows → T0 (keep as a lint); bd set-state rows → MERGE-INTO test-bd-contract.sh |
| 30 | test-ops-allowlist::all | T1, 6 s (sourcing lib.sh) | KEEP. Read `FAYTH_TOOLS` by sourcing the fayth alone, not lib.sh. Extend to czar.fayth. TI: `ops_allows` reimplements the CLI matcher (see Gaps) |
| 31 | test-czar-shadow::all (secondary) | T1, 1 s | KEEP; fix the positive-control bug (below). Move the 11 conf/fayth/brief grep rows to T0 lint |
| 32 | test-spike::confine refuses/allows/plan bead/unreadable/adjacent/deep/merge-base; ::landing lands/refuses | T3, (part of 24 s) | KEEP confine as T2 (git + stub bead-label lookup). Keep the two landing rows as T3 on main only |
| 32 (partition rows) | test-spike::partition (9) | T3 | MERGE-INTO test-fayth-predicates.sh (dispatch area) |
| 33 | test-spike::placeholder, commands, fayth parses, brief clauses, ops wall, spike.fayth fields, reopen lint | T0 in a T3 suite | Move to a T0 `lint-chamber.sh` stage (no DB) |
| 34 | test-freshclone::all (secondary); test-spike::bash -n on fayth/confine | T0, 3 s | MERGE the spike `bash -n` rows INTO test-freshclone (becomes the single syntax lint) |

---

## 4. Duplicate clusters

**D1 — PreToolUse pass-through boilerplate (10 suites).**
- **Evidence:** test-aeon-fence, test-aeon-fence-bd-create, test-bd-close-unacked-guard, test-bd-delivers-label-guard, test-bd-remember-guard, test-bd-update-inflight-guard, test-focus-close-guard, test-mail-fence, test-null-close-reason, test-testenv-batch-fence (plus secondary test-schema-migration-guard) each carry their own copy of the payload builder (`python3` JSON), `env -i` runner, `want/nowant`, and the same six rows: no-`SPIRA_AEON`, non-Bash tool, single-quoted prose, heredoc prose, env override, inline override. The reducers (r0, r1, r2, r3) all independently flagged this cluster.
- **Underlying cause:** each guard script carries its own copy of the quote/heredoc stripping logic. `bd-update-closed-guard.sh` documents its three-step matcher inline, and the others repeat it.
- **Keep:** one `test-guards.sh`. Each guard contributes a rows file `(guard, tool, command, env, expect_rc, expect_stdout_substr)`. The shared pass-through block is applied to every guard automatically.

**D2 — aeon-fence split across two files.**
- **Evidence:** test-aeon-fence and test-aeon-fence-bd-create both drive `hooks/aeon-fence.sh` with the same harness and the same `decision:block` assertion. Inside bd-create, the positive control, A1 and A2 are the same command three times.
- **Keep:** the rows in test-guards.sh, with the command run once.

**D3 — branch-guard.sh staged.**
- **Evidence:** test-branch-guard and test-aeon-worktree-guard exercise the same `branch-guard.sh staged` identity rules, each with its own tmp repo, and both source lib.sh. test-reclaim-slay-branch-guard (landed-audit-reaping) shares only the name.
- **Keep:** test-branch-guard, absorbing the four worktree rows.

**D4 — suite-state lint rules.**
- **Evidence:** the no-bead, missing-suite and missing-reason rules are tested in test-suite-state (T1, 2 s) and again through `suite-state-fence.sh` on a real Dolt DB (test-suite-state-fence, 4 s).
- **Keep:** test-suite-state, plus one CLOSED-bead row with stub bd.

**D5 — the schema_migrations DELETE refusal in two mechanisms.**
- **Evidence:** test-schema-migration-guard (PreToolUse) and test-destructive-bead (bdq create).
- **Keep:** both mechanisms, which are legitimately separate, but share the input strings. Delete the vacuous rows in test-schema-migration-guard (`want` with an empty needle; `ok` called unconditionally).

**D6 — shipped-tree fence re-run.**
- **Evidence:** test-inventory::"every shipped file passes inventory" re-runs what `gate-spira.sh` already runs in its `for fence in …` loop (line 111).
- **Keep:** the gate's run; delete the suite row.

**D7 — wiring greps.**
- **Evidence:** test-inventory, test-scratch-fence and test-suite-state-fence each grep `gate-spira.sh` for their own fence's name, and test-scratch-fence greps `hooks/pre-commit`.
- **Keep:** one behaviour row that reads `gate_fence_list` (from `gate-fences.sh`) and asserts it equals the expected set, plus the UC-22 commit-through-hook row.

**D8 — syntax sweep.**
- **Evidence:** test-freshclone runs `bash -n` over all shipped scripts, and test-spike runs `bash -n` over fayths and `confine.sh` again (a subset).
- **Keep:** test-freshclone, with its sweep widened to include `chamber/*.fayth`.

---

## 5. Unit-extractable logic

| Logic | Tested today through | Seam for T1 |
|---|---|---|
| Close-unacked decision f(payload, env, claim_ts, comments[], notes[]) | test-bd-close-unacked-guard: real Dolt, ~8 bd writes, 6× `sleep 2` to order timestamps, `run_guard` run twice per case | Guard already calls `bd` via `SPIRA_BD`/PATH. A stub `bd` that answers `show --json` / `comments --json` from a fixture dir (`$STUB/<id>.show.json`) with explicit timestamps. No sleeps, no DB |
| Unacked-comment delivery (PostToolUse) | same suite, same fixture | Same stub `bd`, plus a tmp `SPIRA_RUN` for the seen-set (idempotency row) |
| In-flight predicate f(status, assignee, labels) plus spec-flag parse | test-bd-update-inflight-guard: testdb, `bd update --force` to fake in_progress | Stub `bd show --json` with two canned records |
| Focus-close predicate f(cited ids → status, is_insight, `SPIRA_FAYTHS` vs auto roster) | test-focus-close-guard: testdb, and reads the live chamber to choose a focus persona | Stub `bd show` for 3 ids. Point `SPIRA_CHAMBER` at a two-fayth fixture so exit 77 cannot happen |
| delivers-label producer lookup | test-bd-delivers-label-guard: testdb only to own a bead id | Stub `bd`. The "label remains" row goes (vacuous) |
| aeon-fence test-data heuristic | test-aeon-fence-bd-create: testdb only as a path, and the guard shells `bd create --dry-run` | Regex arm is pure. Stub `SPIRA_BD` printing "appears to be test data" for the bd arm. One T2 row in test-bd-contract pins bd's real message |
| Shared command tokenizer (strip heredoc bodies, then single-quoted spans; keep double-quoted) | re-tested inside every guard suite | Extract to `spira/guardlib.py` (each guard already embeds Python). `decide(payload, env, lookup) -> (rc, msg)` becomes callable in-process, so ~200 table rows run in one python process in <1 s instead of ~400 forks |
| scratch-fence root-file rule | test-scratch-fence: git commits per case | Accept the file list on stdin, or `SCRATCH_FENCE_LS=` a command. The git index read stays a single T2 row |
| inventory token scan | test-inventory: git trees | `inventory.sh --scan <file>` already exists. Drive all pattern/exemption rows through `--scan` (T1), and keep one tracked plus one untracked tree-walk row (T2) |
| suite-state CLOSED-bead rule | test-suite-state-fence: real bd create/close | Stub `bd show` returning `{"status":"closed"}` |
| Persona allowlist matcher | test-ops-allowlist: sources lib.sh (6 s) for `fayth_get` | `bash -c '. chamber/ops.fayth; printf %s "$FAYTH_TOOLS"'`. See the TI gap on the matcher itself |
| Dirty-path guard | test-aeon-dirty-commit: podman re-exec; never ran in CI | Host-run with `GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1`. The snapshot is written by `aeon.sh` inline (`_dirty_snapshot`, aeon.sh ~1262). Extract a `dirty_snapshot <work> <out>` function so the test calls the real one instead of an inline copy |
| Spike static lints | test-spike first ~40%, running inside a testdb/git T3 suite | No seam needed: they are pure text reads. Move them to a T0 `lint-chamber.sh` |

---

## 6. Gaps

1. **Seven tested guards have no visible registration (UC-16).**
   - **What is registered:** `aeon_settings()` (`spira/aeon.sh:115-139`) registers exactly three PreToolUse hooks: `hooks/aeon-fence.sh`, `bd-close-unacked-guard.sh` and `bd-delivers-label-guard.sh`.
   - **What is not:** a grep for `mail-fence.sh`, `bd-update-closed-guard.sh`, `bd-remember-guard.sh`, `testenv-batch-fence.sh`, `schema-migration-guard.sh`, `bd-close-focus-guard.sh` and `bd-update-inflight-guard.sh` finds no caller anywhere in the harness tree (outside the scripts themselves and a spike data corpus). It also finds nothing in `brain/.claude/settings.json` or `~/.claude/settings.json`.
   - **What this means:** these guards may be tested but not wired into anything. The thing to check next is the prod host's settings.
   - **Test that would catch it:** a T1 row asserting that the guard set in `aeon_settings` output equals a declared manifest, for both the sweep (aeon.sh:224) and the claimed launch (aeon.sh:1775).
2. **`aeon_settings` fails open silently.**
   - `python3 … 2>/dev/null` plus `_AEON_SETTINGS="$(aeon_settings)" || _AEON_SETTINGS=""` means an aeon runs with no fences if Python fails.
   - The `os.access(..., X_OK)` checks drop a non-executable guard with no message.
   - Untested. It violates the area's own fail-closed dimension.
3. **Worktree hook install failure is swallowed.** `aeon.sh:~1280` runs `worktree-hooks.sh install … >/dev/null 2>&1 || true`. After sp-urifb, a failed install means an aeon worktree with no ref guard and no pre-commit fences. test-ref-guard only asserts that `pre-commit` exists and is executable. It never asserts that the composed hook runs `exclude.sh`, `scratch-fence.sh` and `branch-guard.sh` before `pre-commit-guard.sh` (UC-21).
4. **`exclude.sh` has no suite (UC-23).** It is 336 lines, and it is the first entry both in `gate-spira.sh:111`'s fence list and in `hooks/pre-commit`. `law-beads-is-never-public` rests on it. Test files mention it only as a copied fixture.
5. **The canonical `hooks/pre-commit` is never executed by a test (UC-22).** test-branch-guard copies it and never runs it, and test-scratch-fence greps it.
6. **aeon-fence covers Bash only.** `hooks/aeon-fence.sh:26` (`[ "$tool" = "Bash" ] || exit 0`) means the Write and Edit tools can write into `SPIRA_PROD` or landstate unrefused. mail-fence handles Write/Edit for `SPIRA_MAIL`, so the pattern exists. No test covers Write/Edit against prod.
7. **Malformed or empty payload behaviour is untested for every guard.** aeon-fence returns allow on JSON parse failure (lines 21-33), and the bd-create arm's `bd --dry-run` subprocess swallows every exception (`except Exception: pass`), so a missing or hung `bd` allows the create. Fail-open may be the right policy for a PreToolUse hook, but it should be a decided, tested row rather than an accident.
8. **Two refusal protocols.** aeon-fence blocks with exit 0 plus `{"decision":"block"}` on stdout. The bd-*/mail/testenv/schema guards block with exit 2 plus stderr. test-mail-fence, test-testenv-batch-fence and test-bd-delivers-label-guard assert only the "BLOCKED" text, not the exit code, so a guard that prints but exits 0 would pass. The shared table must assert the protocol column.
9. **test-czar-shadow's positive control cannot fail.** Line 50 is `SPIRA_CZAR_STAGE_DEADLOCK="" "$FENCE" deadlock … && rc=1 || rc=$?`: a fence that wrongly *allows* sets rc=1, which the next line accepts as success. The fix is `rc=0; "$FENCE" deadlock || rc=$?`.
10. **test-aeon-dirty-commit is dead in CI.** It keys on `IN_TESTENV`, while CI sets `SPIRA_IN_TESTENV`, and it re-execs via podman inside testenv. signals shows NA/not-run. A missing `pre-commit-guard.sh` exits 0, which is a skip reported as a pass. sp-rbxr's fence currently has no CI protection.
11. **The allowlist matcher is a test-local reimplementation.** test-ops-allowlist's `ops_allows` approximates Claude CLI `Bash(pattern)` semantics, so a CLI that matches differently passes the test while the real fence differs. There is also no deny-table for `czar.fayth`, whose `FAYTH_TOOLS` includes `Bash(bd *)` and `Bash(*queue.sh*)`. Proposal: one table per restricted persona, and a single T2 contract check against the CLI's own matcher, if it exposes one.
12. **Silent skips.** test-focus-close-guard exits 77 when the live chamber has fewer than 2 auto personas, and test-host-reason's `suites.sh status` number check sits inside an `if` that skips when the line is missing. The shared runner should count 77 as SKIP distinctly, and certification should fail on an unexpected SKIP.
13. **No reporting contract.** Output is `ok`/`FAIL` text with no stable case ids. test-suite-state-fence has no summary line, and test-suite-state uses `ok:`/`FAIL:`. Proposal for the area: the test-guards.sh runner emits one TAP line per row, `ok N - UC-safety-fences-11/redirect-expanded-path`, so rows map back to use cases and runs are comparable.

---

## 7. Cost

**Current.** Sum of ci_secs over the 22 primary files (test-aeon-dirty-commit is NA and counts as 0):

| Group | Files | Current (s) |
|---|---|---|
| PreToolUse guard suites | bd-create 6, aeon-fence 10, unacked 39, delivers 6, remember 9, inflight 9, focus 5, mail 4, null-close 2, testenv-batch 3 | **93** |
| Git hook suites | branch-guard 4, worktree-guard 2, ref-guard 1, dirty-commit 0 (NA) | **7** |
| Lints / static fences | inventory 3, scratch 3, orphan 12, host-reason 14, destructive 4, ops-allowlist 6, set-state-writers 8 | **50** |
| test-spike | | **24** |
| **Total** | | **174 suite-seconds** (≈1.8 % of the ~9,600 s main push) |

Secondary files add 13 s (czar-shadow 1, freshclone 3, schema-migration-guard 3, suite-state-fence 4, suite-state 2). They are not counted above.

**Projected after the verdicts** (primary scope):

| Group | Projection | Seconds |
|---|---|---|
| test-guards.sh (all 10 guard suites merged; stub bd; still one fork per row) | ~130 rows × ~0.15 s under CI load | **20** |
| test-bd-contract.sh (new T2: bd `--dry-run` test-data text, `comments --json` / `show --json` shapes the guards parse, set-state single-valued) | one testdb | **8** |
| Git hooks | branch-guard with worktree rows 5 + ref-guard 1 + dirty-commit on host, now actually running 2 | **8** |
| Lints | inventory 2 (−shipped-tree row) + scratch 3 + orphan 10 (−2 tautological rows) + host-reason 4 (−`suites.sh status` sweep, moved) + destructive 4 + ops-allowlist 2 (no lib.sh) + set-state lint 1 | **26** |
| test-spike | T0 lints 2 + confine T2 3 + landing T3 10; partition rows moved to dispatch | **15** |
| **Total** | 20 + 8 + 8 + 26 + 15 | **77 suite-seconds** |

The saving is 174 − 77 = **97 s (−56 %)**. Of that, 93 − 28 = 65 s comes from the guard cluster (39 s is test-bd-close-unacked-guard alone, 12 s of it `sleep 2`).

With a stage-2 `guardlib.py` in-process decision table, test-guards.sh drops from 20 s to about 2 s and the area total to about **59 s**. About 4 s of `suites.sh status` time moves to test-infrastructure rather than disappearing.

**Tier placement after the change:**
- **T0 / T1 → certification:** test-guards.sh, destructive-bead, host-check, suite-state, ops/czar allowlists, chamber lint, syntax lint. About 30 s, and every commit gets the full guard table.
- **T2 → batch CI:** branch-guard, ref-guard, dirty-commit, scratch, inventory tree-walk, orphan-test, confine, bd-contract. About 35 s.
- **T3 → main CI only:** spike landing rows, about 10 s.

The area is cheap in absolute terms. The case for acting on it is the gaps: 1, 2, 3, 4, 9 and 10 each mean a fence that is untested, not armed, or can pass while broken.

---

## 8. Implementation status (added by sp-pso1u, not part of the approved plan text above)

This area's dependency beads (sp-qvjzb, sp-qu948) were closed but not landed on
`origin/main` when this bead ran — `spira/testlib.sh` and the `docs/test-plan/` T0 lint
the AC assumes (B2, "the area's suites all use testlib") are not yet present to build
on. Per the precedent set by sp-cb39h (landed-audit-reaping) in the same situation,
suites below use the existing per-suite `ok/bad/is/want` convention rather than block on
an unlanded dependency; they should be swept into `testlib.sh` mechanically once
sp-qvjzb lands, the same as every other suite. `# tier:`/`# covers:` headers are added
now regardless, since they cost nothing and need no rework once the lint lands.

**V12 / "the guard system is retired (C4)" — read narrowly, not applied to the two
still-live guards.** sp-fjsxb (landed, e3f873fd) retired 9 of 11 PreToolUse guard
scripts and their suites, but explicitly *kept* `hooks/aeon-fence.sh` and
`bd-close-unacked-guard.sh` as "guards preventing destructive actions with no structural
replacement yet" (sp-kz8ob, sp-mvg44), adding `test-aeon-settings-guard-allowlist.sh` to
pin exactly that allow-list. Taken literally, this bead's review instruction ("delete
the guard suites after C4") would delete `test-aeon-fence.sh`,
`test-aeon-fence-bd-create.sh` and `test-bd-close-unacked-guard.sh` too — but those two
scripts are still live, enforcing code, and deleting their only test coverage would
leave a real security control untested, which is exactly what this area's own
fail-closed and test-integrity dimensions exist to prevent. Read narrowly — C4 is
sp-fjsxb's actually-landed retirement of the 9 orphaned/policy guards, not a claim that
the 2 kept ones are gone too — the literal 9 are deleted (confirmed: none of
test-bd-delivers-label-guard.sh, test-bd-remember-guard.sh,
test-bd-update-inflight-guard.sh, test-focus-close-guard.sh, test-null-close-reason.sh,
test-mail-fence.sh, test-testenv-batch-fence.sh, test-schema-migration-guard.sh exist on
this branch's base), and UC 06–13's MERGE/DEMOTE verdicts above are moot for the same
reason. The remaining two guards' suites (UC 01–05, 14–16) are **kept and not deleted**
in this slice. This read was sent to the operator as a decision to confirm, not assumed
silently (mail: "safety-fences: guard suites — narrow reading of V12/C4, confirm or
correct").

**Landed in this slice:**
- **UC-safety-fences-31, gap 9:** fixed test-czar-shadow.sh's positive-control bug (the
  `&& rc=1 || rc=$?` idiom accepted a wrongly-allowing fence as a pass).
- **UC-safety-fences-25, D6:** deleted test-inventory.sh's "every shipped file passes
  inventory" row — gate-spira.sh:126 already runs `bash spira/inventory.sh` against the
  shipped tree on every landing; the mirrored re-run asserted nothing new, twice, on
  every branch.
- **UC-safety-fences-16:** already covered by `test-aeon-settings-guard-allowlist.sh`
  (added by sp-fjsxb); retrofitted its `# tier:`/`# covers:` header.
- **UC-safety-fences-23, gap 4:** new `spira/test-exclude.sh` (T2). Fail-closed rows
  first (no harness signature in the path list), then the positive control (every
  forbidden shape flagged), clean paths, and `in_scope` narrowing. Writing it surfaced a
  real product defect rather than a test gap: `filter`'s scope is never widened the way
  `scope`/`check`/`staged` are, so with the harness's current one-level-under-root
  layout, `git ls-files | exclude.sh filter` — gate-spira.sh's own landing-gate call —
  never scans the repository root, while `check` and the pre-commit `staged` hook both
  do. Filed as **sp-aoads** (P1, discovered-from this bead) rather than fixed silently;
  the suite documents the current (buggy) behaviour with a citation to that bead.

**Deferred to follow-up beads, filed against this area with the relevant sections above
as their brief:**
- **sp-qsr44** — D1/D2 `test-guards.sh` consolidation (UC 01–05, 14–16; gaps 1, 2, 6, 7,
  8, 12, 13), pending the operator's confirmation of the V12/C4 reading above. UC 06–13
  are moot (their guards no longer exist after sp-fjsxb).
- **sp-pohf2** — D3 git-hook suite merge (UC 17–22; gaps 3, 5): fold
  test-aeon-worktree-guard into test-branch-guard, extend test-ref-guard for the
  composed pre-commit hook, and drive the canonical `hooks/pre-commit` directly.
- **sp-rv8jt** — D4/D7/D8 lint consolidation (UC 22's wiring-grep half, UC 28, UC 33,
  UC 34): a `gate-fences.sh` module exposing `gate_fence_list`, widening
  test-freshclone's syntax sweep to absorb test-spike's `bash -n` rows, extracting the
  chamber-integrity lints into a standalone T0 stage, and merging
  test-suite-state-fence's structural rows into test-suite-state.
- **sp-av1xy** — Persona scoping and spike confinement (UC 24, 26, 27, 29–32; gaps 10,
  11): host-check.sh's `suites.sh status` sweep move, the allowlist-matcher TI gap, and
  the spike/dispatch UC-32 partition-row move. (test-aeon-dirty-commit's CI-dead fix,
  gap 10, moved into sp-pohf2 alongside UC-17/UC-21 since it's the same worktree-hook
  seam.)

`B2` (testlib migration) is deferred for the whole area, as above, tracked implicitly by
sp-qvjzb's own landing rather than a separate bead here.
