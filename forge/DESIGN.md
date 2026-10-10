# forge — the one seam onto GitHub

Replaces `spira/forge.sh` (724 lines). One binary, `forge`, the same twenty-one verbs plus
two new ones the pr-pass port needed (`pr-list-open`, `pr-automerge`). This document is the
contract; it was written from the script's intent and its callers (bd sp-t4y60), not by
porting it line by line. The code satisfies it and the unit tests are derived from it.

## 1. Intent

Every place Spira asks or tells GitHub something — is a branch's gate green, is its pull
request open, cancel this run, protect this branch — goes through one seam so there is one
place that knows how to reach GitHub and one place that is careful about what "unknown"
means. `forge` is that seam, ported from bash to Rust with **the same credential path,
unchanged**: it execs `${SPIRA_GH:-gh}` under `timeout ${GH_TIMEOUT:-120}`, exactly as the
bash `ghq()` wrapper did. `SPIRA_GH` is usually `gh-app.sh`, which asks `broker token` for a
GitHub App installation token and execs the real `gh` with `GH_TOKEN` set; `forge` never
reads a token, a key or the broker's output itself — it only ever launches the same program
bash used to launch, with the same argv shape. Nothing about *who Spira is on GitHub* moves.

## 2. What is deliberately NOT touched

- **The credential path.** `forge` does not call the broker, does not read
  `SPIRA_GH_APP_*`, does not touch `GH_TOKEN`. It launches `${SPIRA_GH:-gh}` and lets that
  program (real `gh`, or `gh-app.sh`) decide.
- **Artifact zip reading.** `_forge_artifact_zip` / `_forge_red_suites_artifact` /
  `_forge_fail_lines_artifact` read a downloaded GitHub Actions artifact zip (`results.jsonl`
  or the older `red-suites.json`, and per-suite `.out` files) via `python3 -c` scripts. That
  logic is unchanged, ported byte-for-byte as the same embedded Python (`src/zipread.rs`),
  run as a subprocess exactly as bash ran it. A `zip`-crate rewrite was considered and
  rejected: the artifact format and its two-generation fallback are exactly specified by the
  existing Python, re-deriving it in Rust risks a silent behavioural drift for no reader
  anywhere that needs it to be Rust, and `python3` is already a hard dependency of this
  tree (`bdjson`, `forge.sh` itself, a dozen suites).

## 3. Contract

### 3.1 Invocation

`forge <verb> <repo-dir> [args...]`, exactly forge.sh's argv shape. `repo-dir` is a path;
most verbs run `gh` with that as the working directory (a `cd "$repo" && gh ...` in bash
terms). Body payloads that were `stdin` in bash (`pr-create`) stay stdin.

| verb | args | prints |
|---|---|---|
| `pr-create` | `<head> <base> <title>`, body on stdin | the new PR number |
| `pr-number` | `<head>` | the open PR number, or empty |
| `pr-list-queue` | — | PR numbers with head `spira/queue/*`, one per line |
| `pr-list-open` | — | **new**: `<number> <headRefName>` per open PR, one per line |
| `pr-mergeability` | `<pr-number>` | `DIRTY \| CLEAN \| UNKNOWN` |
| `pr-state` | `<pr-number-or-branch>` | `open \| merged \| closed \| unknown` |
| `pr-red` | `<pr-number-or-branch>` | `head <sha>`, then `job <name>` and `fail-line: <text>` per failing check; nothing when not red or unreadable |
| `pr-automerge` | `<pr-number-or-branch>` | **new**: arms squash auto-merge; exit 0/1, silent |
| `check-status` | `<pr-number> <branch>` | `pending\|green\|red\|harness_fault\|provision_fault`, then `head-sha:`/`run-url:`/`build-error:` lines, then `red-suite:`/`flaky:` lines when red |
| `batch-ci-status` | `<branch>` | `run-id:`/`run-conclusion:`/`run-completed-at:`/`head-sha:`/`run-url:`/`queued-since:` then red-suite/flaky lines |
| `run-id` | `<branch>` | latest CI run ID |
| `runs-for-branch` | `<branch>` | `<id> <status>` per non-completed Gate run |
| `runs-queue-branches` | — | `<id> <branch> <status>` per non-completed Gate run on `spira/queue/*` |
| `runs-active` | — | count of queued+in_progress PR runs, or `?` |
| `queued-since` | `<branch>` | earliest queued job's epoch, or nothing |
| `run-metadata` | `<run-id>` | `started-at:`/`last-activity:` |
| `run-cancel` | `<run-id>` | cancels; silent |
| `workflow-rerun` | `<run-id>` | re-queues a failed run; silent |
| `dispatch` | `<ref> <suites-csv>` | triggers a Gate run restricted to `suites-csv`; silent |
| `fail-lines` | `<run-id> <suites-space-sep>` | `fail-line: <suite>: <text>` |
| `pr-close` | `<pr-number>` | silent |
| `pr-comment` | `<pr-number> <body>` | silent |
| `branch-protect` | `<base-branch>` | GitHub's response (stdout+stderr merged, as bash did) |
| `branch-protection-status` | `<base-branch>` | `protected \| unprotected` |

### 3.2 Fail closed

Every verb that answers a yes/no or a status prints the "cannot tell" answer — never a
guess — when `gh` fails, times out, or returns unparseable JSON: `pending` for
`check-status` with no run, `UNKNOWN` for `pr-mergeability`, `unknown` for `pr-state`,
`unprotected` for `branch-protection-status` (matching bash: an unreadable branch is not
provably protected), `?` for `runs-active` (never `0` — absence of proof is not proof of
absence, `law-absence-needs-a-positive-control`), empty for id-returning verbs. This was
already forge.sh's own discipline; the port keeps it and the unit tests pin it per verb.

### 3.3 Environment

`SPIRA_GH` (default `gh`), `GH_TIMEOUT` (default `120`), `SPIRA_QUEUE_ACTIONS_APP_ID`
(default `15368`, `branch-protect` only). Nothing else. `forge` reads no config file and no
repo map — every caller already resolved which repository directory to pass.

## 4. Schema

```rust
/// One `gh` invocation: repo-scoped (cwd) or not; stdout only, or stdout+stderr merged
/// (branch-protect matches bash's `2>&1`).
trait Gh {
    fn call(&self, repo: Option<&Path>, args: &[&str]) -> GhOut;   // stderr discarded
    fn call_merged(&self, repo: Option<&Path>, args: &[&str], input: Option<&[u8]>) -> GhOut;
}
struct GhOut { code: i32, stdout: Vec<u8> }
```

`RealGh` execs `timeout <GH_TIMEOUT> <SPIRA_GH> <args...>` with `current_dir` set when
`repo` is `Some` — the same two-process shape `ghq()` had (`timeout` wraps the gh-like
binary). Unit tests use a `FakeGh` that records calls and returns canned JSON, the same
technique the bash suites used (a stub `gh` script), now in-process instead of PATH-swapped.

## 5. What moved and what didn't

- `pr_state`/`pr_merged`/`land_pr`/`needs_refresh` (lib.sh) call `ghq` directly, bypassing
  forge.sh. This port does not change lib.sh — those four stay bash for callers still on the
  landing.sh/pr-pass-branch.sh path. **landing-pass's new pr-branch verb (sp-t4y60, the same
  bead) calls `forge pr-state`/`forge pr-create`/`forge pr-list-open`/`forge pr-automerge`
  instead of shelling to `ghq` a second, parallel way** — one seam onto GitHub, not two.
- `queue_cancel_branch_runs`/`queue_sweep_orphan_runs` (lib.sh) and every bash caller
  (`verdict.sh`, `batch.sh`) keep calling "the forge" by the `SPIRA_FORGE`-named program;
  only `conf.sh`'s default changes, from `forge.sh` to `forge` (bare name, sp-gypjk). No
  caller assumed a `.sh` extension or ran it under `bash` explicitly (audited).

## 6. Decisions

- **Two new verbs** (`pr-list-open`, `pr-automerge`) were added rather than overloading
  existing ones, because `land_pr`'s dedup scan and auto-merge arm have no forge.sh
  equivalent today and adding a verb cannot break an existing caller.
- **Artifact zip reading stays Python**, run as a subprocess (§2). Everything around it —
  the `gh api` calls, control flow, output formatting — is native Rust.
- **`pr-state` accepts a PR number or a branch name**, unchanged from forge.sh: `gh pr view`
  itself accepts either, so no signature split was needed for lib.sh's branch-keyed callers
  to become verb-compatible.
