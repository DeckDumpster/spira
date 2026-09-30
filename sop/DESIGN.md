# sop — the Standard Operating Procedure shelf

Replaces `spira/sop.sh` (865 lines, deleted, sp-8fsql). One binary, `sop`, the same eleven
subcommands and the same environment variables, minus `SOP_SHELF_CMD` (§4). Written from the
script's intent and the header comment it already carried — the bash's own header already
explained *why* almost everything here is shaped the way it is; this crate keeps that
explanation and makes the promises it stated actually hold (§4.1).

## 1. Intent

Statutes are how to behave; SOPs are how to fix. Both live in the same mechanism — `bd
remember`/`bd recall`, split by key prefix (`law-`/`sop-`) — so Ops reads runbooks exactly
the way every agent already reads statutes, rather than through a second knowledge channel.
An SOP is four required-or-optional fields (`SYMPTOM`, `CHECK`, `FIX` required; `MATCH`,
`METRIC`, `ESCALATE`, `REF` optional), never prose, because prose cannot be matched to an
incident by a program and cannot be executed without being re-read. `applied` is the closing
half: a session that matched a runbook and ignored it must leave a different trace than one
that executed it faithfully, so every application is recorded twice — a ledger line (for
counting) and a bead note (for the human reading the incident later) — and `--held yes` is a
first-class, stated outcome, never inferred from silence.

## 2. What is deliberately NOT touched

- **`bdq`/`bdjson`, via `lib.sh`, never called directly.** `bdq` carries real safety logic
  this rewrite does not reimplement: a repo-label vocabulary check, a destructive-SQL
  refusal, the czar-fence hook on `reopen`/`update`/`close`, and a retry around a Dolt
  connection the server already dropped. `spira/lib.sh` is last in the rewrite order (the
  inventory, group 4) and Ryan's standing instruction during the cutover was "leave lib.sh
  alone." `real.rs`'s `RealBd` shells out to it — `bash -c '. "$0" ...; bdq ...' lib.sh
  <args>` — the identical seam `gate-check`'s Rust port already draws around `repo_root`.
  Arguments are passed as real argv entries (`"$1"`, `"$2"`, ...), never interpolated into
  the script text, so an SOP's own text can never be read as shell syntax.
- **`spira-lint --only inventory --scan /dev/stdin`**, unchanged: the operator-infrastructure
  check an SOP's text is held to at `write`/`validate` time.
- **The METRIC probe's binary**, still whatever `SOP_METRIC_COCKPIT` names or `cockpit.sh` by
  default (group 5, operator surface — rewrite when next touched).
- **`mail.sh`, `bd`, `git`** — all still bash/external, called by bare name exactly as before.

## 3. Contract

Subcommands, argv and environment variables are unchanged from `sop.sh`'s own header
(`SPIRA_DB` implicitly, via `lib.sh`; `SPIRA_HOME` to locate it; `SOP_WORD_CAP` default 250;
`SOP_WHY_CAP` default 400; `SPIRA_SOP_LEDGER` default `$SPIRA_RUN/sop/applied.jsonl`;
`SOP_PAGE`/`SPIRA_WIKI` for `synth`'s output path; `SPIRA_TZ`/`TZ` for `synth`'s date;
`SOP_METRIC_COCKPIT` for the METRIC probe override; `BEADS_ACTOR`/`SPIRA_AEON`/`USER` for the
recorded actor).

### 3.1 Module map

- `validate.rs` — the one shape-validator (§4.1).
- `shelf.rs` — parses `bdjson memories`'s raw JSON into the `sop-*` string map every
  subcommand reads, in the two flavours the bash had: strict (`parse`, `None` on any
  unreadable input — `digest`/`lint`/`applied`'s slug check) and lenient (`parse_or_empty`,
  silently empty on failure — `list`/`match`/`synth`, exactly as the bash's own `shelf()`
  swallowed a parse error into `{}`).
- `match_sop.rs` — `match`'s scoring: MATCH regex over distinct payload lines, or key-token
  fallback when there is no MATCH line.
- `ledger.rs` — one applications-ledger record (JSON line, `why` normalised and capped) and
  `log`'s three-valued read (found / genuinely empty / corrupt).
- `synth.rs` — pure markdown rendering of the shelf into the wiki page's text.
- `ports.rs`/`real.rs` — `Bd`, `Proc` (spira-lint scan, METRIC probe), `Clock`; the real
  implementations are the only place anything shells out or touches the filesystem/clock.
- `logic.rs` — one function per subcommand, over the ports above; every test in
  `logic/tests.rs` drives these through fakes.

### 3.2 `write`

Refuses empty text, a shape violation (§4.1), or operator infrastructure named in the text
(`spira-lint --only inventory`). Otherwise `bdq remember`s it and regenerates the wiki page.

### 3.3 `applied`

Unchanged decision tree: refuse missing `--bead`/`--pass`, an unspellable `--check`/`--held`,
or `--check fail --held yes` (a CHECK that did not confirm cannot have held). Read the shelf
(strict); refuse an unknown slug only when the shelf was actually readable. When
`--held yes` and the shelf is readable, re-check any `METRIC: KEY SUBCMD` field via the
cockpit probe and downgrade `held` to `unknown` if the metric has not actually cleared
(law-absence-needs-a-positive-control: a metric that cannot be read is not reporting zero).
Write the bead note (when `--bead`) and the ledger line (always), in that order, and report
whether the note actually landed.

### 3.4 `synth`

Resolves the output path exactly as the bash did: `SOP_PAGE` override, else
`$SPIRA_WIKI/wiki/notes/standard-operating-procedures.md`, UNLESS the current directory is
itself a worktree of `SPIRA_WIKI` (compared by `git rev-parse --git-common-dir`, never by
path string — a worktree and its origin checkout are one repository under two paths).
`resolve_out_path`/`wiki_worktree_root` in `real.rs` do this with real `git` calls; nothing
about it is mocked, because the git-common-dir comparison IS the logic, not a detail behind a
seam.

## 4. Parity — named differences from the bash

### 4.1 `write` now enforces the SAME shape as `lint`/`validate` — a dropped accreted accident

The bash's `write` and its `_sop_validate` (reached from `lint`/`validate`) enforced
*slightly* different rules: `write` checked `SYMPTOM`/`CHECK`/`FIX`, the MATCH regex, and the
word cap inline; only `_sop_validate` also checked `METRIC`'s `KEY SUBCMD` shape. A malformed
`METRIC:` line could be written clean through `write` and only be caught later by `lint` — or
never, if `lint` was not run. The bash's own header already promised "the one validator ...
no shelf, no bd" for `_sop_validate`; this rewrite delivers that promise literally:
`validate::validate()` is now the ONE function `write`, `lint` and the `validate` subcommand
all call, METRIC included. Any SOP that `write` refused before still refuses; no SOP that
used to write clean now refuses UNLESS its `METRIC:` line was malformed, in which case it now
refuses at write time instead of silently shipping a structurally broken field.

### 4.2 `SOP_SHELF_CMD` is dropped

The bash's test seam that substituted `bd memories --json`'s output entirely is replaced by
real dependency injection (`ports::Bd`, faked in `logic/tests.rs`) — the same trade `gh-intake`
made for its `GH_INTAKE_LIB=1` seam. Nothing in production ever set `SOP_SHELF_CMD`; only
`test-sop.sh` did, and that suite is retired in this same change (§5).

### 4.3 Everything else is unchanged

Every subcommand's argv shape, every environment variable and its default, the ledger's JSON
shape and three-valued read, the digest hash, the match-scoring rule, the closing-rule note
text, and the wiki page's rendered structure are load-bearing and preserved.

## 5. Tests

`cargo test -p sop`: 57 tests across `validate.rs` (shape rules, including the unified METRIC
check), `shelf.rs` (strict vs. lenient parsing), `match_sop.rs` (MATCH scoring, key-token
fallback, a bad regex never firing), `ledger.rs` (record shape, the three-valued log read,
digest stability), `synth.rs` (field extraction without lookahead — the `regex` crate has
none — and page rendering), and `logic/tests.rs` (every subcommand's decision tree, including
the METRIC downgrade on both sides, the unreadable-shelf paths, and idempotent slugification).
No real `bd`, no real `lib.sh`, no real filesystem beyond a scratch tempdir for ledger/synth
output.

`spira/test-sop.sh` (89 assertions), `spira/test-sop-lint.sh` and
`spira/test-sop-gate-wired.sh` are retired: their subject is `sop.sh`, which no longer
exists, and every case they carried that was still live is now a `cargo test -p sop` case
(named individually in `docs/test-plan/operator-channel.md`'s `UC-operator-channel-44/45`
markers). `spira/test-sop.sh` is replaced by a thin T1 wiring smoke test of the real binary
(env vars, the `lib.sh` subprocess seam, a real ledger file) — see its own header for what it
covers that the cargo suite cannot. `spira/test-cockpit-sop.sh` is unaffected: it tests
`cockpit.sh`'s own ledger-reading cockpit metrics, not `sop` itself, and still reads the same
ledger file shape this crate writes.

## 6. Callers repointed

- `spira/gate-suites`, `spira/chamber/concierge.md`, `spira/chamber/ops.md`,
  `spira/chamber/ops.fayth`, `README.md`: every `sop.sh <subcommand>` instruction is now
  `sop <subcommand>` — bare name, same as `forge`/`czar-pass`/`gh-intake` before it.
- `aeon/src/verdict.rs`, `aeon/src/run.rs`, `aeon/src/brief.rs`: the string literal `"sop.sh"`
  (used both as an argv-executable name in `aeon`'s own port checks and in the brief text
  rendered to the Ops persona) becomes `"sop"`.
- `spira/watchtower.sh`, `spira/incident.sh`, `spira/lib.sh`: comment-only mentions updated
  for accuracy.
- `spira/test-guards.sh`, `spira/test-aeon-wiki-dirty.sh`, `spira/test-lint-chamber.sh`,
  `spira/test-render-memories.sh`, `spira/test-ops-closing.sh`: repointed to invoke `sop` by
  binary name rather than `$HERE/sop.sh`.
