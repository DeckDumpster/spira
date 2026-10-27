# Development

## Repository layout

| Path | Contents |
|---|---|
| `Cargo.toml` | one workspace of about eighty member crates, each a binary and/or library; most carry a `DESIGN.md` stating their contract |
| `spira-config`, `lifecycle`, `spira-lc`, `beads-store` | config, the lifecycle machines and their service, the bead-store seam |
| `sentinel`, `aeon`, `landing-pass`, `queue`, `batcher-cut`, `gate*`, `testenv`, `release`, `install` | the loop: dispatch, aeon runner, landing, rounds, gate, test runner, releases, installer |
| `loom`, `cockpit/`, `cockpit-collect`, `watchd`, `mail`, `doctor`, `census`, `strand` | read surfaces and operator tools |
| `spira/` | shell not yet ported, `conf.sh` / `lib.sh`, the registry (`conf.d/`, `conf.toml.d/`), personas (`chamber/`), hooks (`hooks/`), `deps.toml`, `watchers`, and every suite (`test-*.sh`) |
| `systemd/` | unit templates (`@PLACEHOLDER@` substitution; `.service` + `.timer` pairs) |
| `docs/test-plan/*.toml` | the use-case catalogue suites declare coverage against |
| `gate.steps` | this repository's certification gate |
| `flake.nix`, `rust-toolchain.toml`, `Makefile` | pinned tools, pinned compiler, build entry points |

The ownership boundary between this repository and everything that is not it is generated from
`spira/boundary` and kept below; amend that file and run `bash spira/boundary.sh write`, never
edit the region.

<!-- BOUNDARY:BEGIN -->

### Ships in `spira` — the harness

Generic mechanism. A colleague clones this and it carries none of the operator's data.

| path | what it is |
|---|---|
| `spira/` | the harness proper — aeon runner, sentinel and its checks, gate, sending, strand, the chamber and its fayth format, lib.sh, and the test suites that hold them |
| `spira/boundary` | this manifest — it describes the harness, so it travels with it |
| `spira/boundary.sh` | renders this manifest into every document that publishes it; the wiki-side target is configured and skipped when unset, per rule 2 |
| `spira/conf.sh` | the one configuration surface: the loader, the key allowlist, the derived defaults, and `spira_require`, which names a missing program instead of dying as a shell error |
| `spira/exclude.sh` | keeps the beads database and its exports out of this repository — the check the gate runs, the pre-commit hook, and the installer that arms both. A beads database is never public |
| `spira/hooks/` | the pre-commit hook itself, TRACKED and armed by core.hooksPath. .git/hooks is not cloned, so a hook that lived there would reach a colleague missing and unannounced |
| `spira-lint/src/rules/inventory.rs` | the fence that keeps one operator's infrastructure out of a repository meant to be cloned — repository names, hosts, paths, people, dates. It scans comments, which is where all of it was. |
| `spira/inventory-deny` | the tokens that fence refuses beyond the structural ones. Ships EMPTY: a list of somebody else's names is itself the inventory |
| `spira/actors.example` | commit author to harness, for authors the commit graph cannot vote on. Its rows are one installation's roster |
| `heimdall/` | the watchdog over the loop (Rust) — reads timestamps and counters, raises or clears an alert bead. Its only power is speech: it repairs nothing, restarts nothing and summons nothing |
| `skew/` | is the activated release the latest published (Rust) — the hourly check that the installed release matches the most recent release tag; also the landing gate's fence against work landing in a copy nothing executes |
| `doctor/` | read-only preflight (Rust) — every missing program, unreadable database, unmapped repository and unbuilt panel, named in one pass |
| `census/` | the Mimir census (Rust) — failure classes ranked by frequency from the events table, with open-remedy suppression |
| `spira/incident.sh` | turns a production event into a bead Ops can claim — systemd OnFailure, arbitrary payload, or a spool drain; deduplicates by external_ref |
| `spira/statutes/` | the SEED statute book, one file per statute. Statutes live in the beads KV store, which is per-installation, so a clone gets the mechanism and none of the law unless it ships as text |
| `spira/seed.sh` | writes those statutes into a fresh database, and never over one already in force |
| `spira/bare-main.sh` | operator tool: `check` or `apply [repo]` makes a repository's main worktree bare while its linked worktrees stay working |
| `spira/otel-query.sh` | operator tool: `report` or `sql [hours]` — DuckDB views over the OTLP span files spira-otel writes (verb stats, large responses) |
| `spira/round-spill-profile.sh` | operator tool: `ensure` or `status` prepares the AWS profile and tailscale key location the EC2 spill provider reads, never a credential |
| `cockpit/` | the decisions panel (Rust) and the ops pane — how a human sees what the harness is doing and answers what it asks. Generic; it reads whatever database it is pointed at |
| `loom/` | Loom — a Rust read endpoint over the live beads graph with a per-request budget; refuses to start without a database so it cannot silently serve another harness's graph |
| `systemd/` | unit TEMPLATES. The units in force on a machine are generated from these, never edited in place |
| `install/` | the installer crate (spira-install, units-install, unit-ensure, render-unit): nine sequential phases — preflight, conflict checks, config, build, database, units, hooks, cockpit, verify — ending with ready.sh |
| `spira/uninstall.sh` | inverse of install.sh; walks owned.sh so the two cannot drift; instance-aware and refuses to guess when multiple instances are installed |
| `spira/owned.sh` | single declaration of what one installation owns outside the checkout — the load-bearing contract walked by both installer and uninstaller |
| `spira/ready.sh` | postflight: seven readiness checks after install; exit 3 when installed-but-not-ready |
| `spira/build.sh` | builds Loom and the cockpit panel; a clone that skips this gets a service that exits 2 and an empty panel |
| `spira/configure.sh` | bootstraps ~/.config/spira/spira.toml on first install; never overwrites an existing file |
| `testenv/src/container.rs` | rootless podman container with user systemd for the install-rehearsal suite tier |
| `docs/` | spike investigations and evidence files from the harness's own development |
| `concierge.sh` | one named Remote Control session, so a phone can reach the harness |
| `rule.sh` | enacting a statute writes the beads KV store, which is the harness's substrate |
| `spira/archive.sh` | keeps every session transcript and indexes it by time range and by the lineage id that survives a clear. The mechanism ships; the transcripts and the store they land in are the operator's own and stay out of every repository |
| `spira.toml.example` | the annotated template an operator copies to spira.toml, the one configuration source ($SPIRA_TOML) |
| `README.md` | the harness's own entry point, carrying this table |
| `AGENTS.md`, `CLAUDE.md` | how an agent works ON the harness. Distinct from the wiki repository's own agent instructions, which are how an overseer works WITH it |

### Stays in `brain` — the wiki

Everything whose write target is a page. It may read the harness freely; the harness may not require it.

| path | what it is |
|---|---|
| `wiki/` | the operator's knowledge base: pages, journal, projects, notes |
| `raw/` | source documents, and any JSONL mirror of the database kept in a PRIVATE repository |
| the wiki's own agent instructions | its catalog, its chronological log, and how an overseer works with the harness |
| a tasks generator | regenerates a page listing the beads that need the operator |
| a statute-book generator | regenerates the readable copy of the statutes in force |
| its cron wrapper | runs that generator on a timer and commits only on substantive change |
| a beads exporter | writes the JSONL mirror into a private repository |
| a bead-table generator | regenerates a plan table on a wiki page from the bead graph |
| that repository's own gate | one repository's landing gate, named by its row in repo-map. Every repository declares its own |
| a checkbox ticker | ticks a checkbox on the wiki page that holds it — the one cockpit tool that writes a page |
| its session guards | PreToolUse fences for the overseer's own session. A guard binds the actor who broke the rule, so they live with that actor |
| its session settings | that session's own hooks and environment |
| its skills | whatever lands its output in the wiki |

### In neither repository — data

It belongs to whoever runs the harness. No shared repository holds it, and no beads database is ever public.

| path | what it is |
|---|---|
| the beads database | served by Dolt, addressed as a path. It accumulates internal working notes and agent memories, so it is never public and never in a shared repo |
| the statutes in force | rows in that database's KV store, per-installation. A wiki may render a read-only copy; the harness ships SEED statute text an installer writes into a fresh database |
| `spira.conf`, `repo-map` | the operator's real paths, repositories and personas. The examples ship; these do not. Both are gitignored, and spira.conf is looked for outside the checkout first for that reason |
| the systemd units in force | rendered from systemd/ templates by install.sh, filled from spira.conf. Never edited in place — `systemd/install.sh --diff` is how you find out somebody did |
| the transcript archive | compressed session logs plus their index, written by archive.sh. They carry paths, credentials read aloud and everything anyone ever said, so they live outside every checkout and no shared repository holds them. Nothing deletes them: retention is the operator's decision |
| `.runtime/` | logs, worktrees, leases, cockpit state. Regenerated, machine-local, gitignored |

<!-- BOUNDARY:END -->

## Building

```sh
make build     # cargo build --release --workspace
make test      # cargo test --workspace
make dist      # build + spira/build-tarball.sh build --workspace .
```

Profiles (`Cargo.toml`): `release` (opt-level 2, no LTO, stripped; what a release ships),
`aeon` (inherits `dev`, incremental; what aeon worktrees and the gate build, with the fence
tools such as `spira-lint` and `lifecycle-guard` optimised within it), and `unittest`
(workspace members unoptimised, dependencies as release builds them; the round's unit-test
build). `testenv --profile <p>` selects one; `--with-bins` means `release`. The toolchain is
pinned by `rust-toolchain.toml`.

## Tests

Tests run through `testenv`, which stages the tree and runs suites in a podman container.
Never run a `spira/test-*.sh` suite directly on the host.

```sh
testenv [--mode parallel|serial] [--suites <list|->] [--profile <p>] [--with-bins] [--artifacts <dir>] [--deadline <secs>] [--report [N]] <branch> [<repo-name>]
testenv --suites test-asks.sh,test-bead-file.sh <branch>
testenv status [--json] <results-dir>
```

`testenv testdb …` manages throwaway Dolt instances for suites that need the real engine.
Rust unit tests run with `cargo test -p <crate>`; `work crates` prints the `-p` words for your
branch's diff.

- **Suites** are `spira/test-*.sh`, found by glob. Each carries `# tier: T0..T4` and a
  `# covers:` line (space-separated path globs and use-case ids from `docs/test-plan/*.toml`)
  just above `set -uo pipefail`. A glob that matches nothing is refused (`covers-entries`); a
  use case no suite covers is reported by `plan-matrix`. A suite that cannot run in a container
  carries `# host-reason: <why>` (`bash spira/host-check.sh` enforces container-or-reason);
  `# bare-host: <why>` marks the few that need the runner itself.
- **Selection.** `suite-select gate <base> <head>` maps a diff to suites through their
  `# covers:` lines; a changed source file that no suite claims is an error.
  `suite-select header covers|tier|uc|requires|exclusive|selects-on|testenv-unmet <file>` reads
  one suite's headers. The landing gate runs that selection under a budget, from `gate.steps`.
- **Two places a suite runs.** `spira/gate-suites` is the only hand-written list: a short,
  reasoned set. Everything the glob finds that it does not name is run on a schedule by
  `testenv suites` and files a bead per red without blocking a landing. `testenv suites
  list|names|corpus|status|hygiene|lint|observe-flake|quarantine|unquarantine|disable|activate`
  reads and changes this; a suite's lifecycle state lives in `spira/suite-state` and changes
  only through the queue like any other change.
- **Use-case catalogue.** `test-plan validate --catalogue-dir docs/test-plan [--suites FILE|-]`,
  `test-plan matrix …`, `test-plan render`, `test-plan schema catalogue|matrix`.
- **Habits every suite shares.** A check that finds nothing must first prove it could have
  found something (plant an offender). Test against the real dependency (`spira/testdb.sh`),
  not a hand-written stub. Run in an explicit, minimal environment, so ambient configuration
  cannot decide a verdict. Pin a configured value to a non-default in fixtures, so a literal in
  the code cannot pass.

## How a change lands

A change to Spira is work like any other. File a bead for the harness repository
(`bead.sh file "<title>" --for builder --repo <name>`); a builder aeon claims it in a worktree
on `spira/<id>`, implements it, runs only the touched crates' unit tests, and calls
`work submit`. The commit subject names the bead id. The sentinel, screen, gate and round carry
it from there ([architecture](architecture.md)), and a release is built, verified and activated from the landed
commit. For a repository in `pr` mode, open the pull request instead.

When working by hand, arm the tracked hooks once with `bash spira/exclude.sh install [root]`
(writes the `.gitignore` stanza and points `core.hooksPath` at `spira/hooks`). The pre-commit
hook runs `exclude.sh staged` (refuses beads data), the cheap lint rules, and
`branch-guard.sh staged` (refuses aeon-identity commits on the base); a `reference-transaction`
hook refuses deleting a bead branch outside the destruction chokepoint; `aeon-fence.sh` is the
PreToolUse hook that blocks queue-operating and credential-touching commands inside aeon
sessions.

## Fences and conventions

`gate.steps` is read from the tree being judged, one directive per line: `bin VAR package`
(build that package from the tree and use it instead of the installed one) and `step
<command>` (an element of the `&&` chain). This repository's steps are `spira-lint`,
`lifecycle-guard --gate .`, `spira/build-fence.sh`, `spira/ci-spill-guard.sh --gate .`,
`spira/boundary.sh check`, then the selected suites. Every fence prints `fence: <name> checked
<n> <unit>` with n > 0; a fence that checked nothing is a failure.

**`spira-lint`** exits 0 clean, 1 findings, 2 usage, 3 a rule refused to report clean, 4 the
tree could not be walked. Rules are discovered from `spira-lint/src/rules/*.rs`; the
shrink-only allow ledgers live beside them. By theme:

- *Config discipline:* only `spira-config` touches `spira.toml` or the repo map
  (`config-fence`); a registered key is read through `spira_config::process::cfg`, never an
  environment fallback or a literal (`config-env-read`, `config-literal-fallback`,
  `conf-key-registry`); a new key needs its schema field, fixture entry, history line and a
  release delta entry (`config-delta`, `config-fixture`).
- *One door per state:* an aeon's phase, a gate's status, a pool, a release's state, a round's
  record, a sift verdict, a testenv result and liveness are read through their owning command
  (`spira-lc`, `gate status`, `round-vm status --json`, `release status --json`,
  `queue round status`, `sift status --json`, `testenv status --json`, `watchd status`), never
  from their files, logs or process lists; nothing reads the journal (`aeon-state-readers`,
  `gate-state-readers`, `pool-state-readers`, `release-state-readers`, `round-record-readers`,
  `sift-state-readers`, `testenv-result-readers`, `liveness-readers`, `journal-reads`).
- *Hygiene:* no installation inventory in tracked files (`inventory`, deny list
  `spira/inventory-deny`, which ships empty); no scratch notes at the repository root
  (`scratch-fence`); every external program declared in `spira/deps.toml` (`deps-lint`); one
  language runtime per repository (`one-runtime`); no download-and-run installs; every
  unattended call has a deadline (`call-deadline`) and every wait is on an object, not a clock
  (`time-only-wait`); payloads go on stdin (`payload-argv-lint`); hash-ordered output is
  sorted; tmux calls are scoped (`tmux-scope-fence`); the Concierge is signalled by inbox,
  never keystrokes (`concierge-send-keys-fence`).
- *Test hygiene:* tier budgets, `wall-clock-budget`, `tmp-leak`, `env-set-var-leak`,
  `chmod-exec-leak`, `testlib-migrated`, `testdb-mode-lint`, `plan-lint`, `plan-matrix`,
  `covers-entries`.

**`lifecycle-guard [--json] [--gate] [--rules <file>] <path>...`** refuses any path that
reaches bead state by a route other than `spira-lc`. It has no allow-list: a finding is cleared
by moving the access to `spira-lc`, deleting it, or arguing a named exception in the owning
crate's design.

**`bash spira/boundary.sh check`** fails when the generated region above is stale.

**Conventions** (from `CLAUDE.md`, which governs anyone editing the harness):

- *Ship the mechanism, not the inventory.* The repository is cloned by people whose
  infrastructure is not yours: no repository, host, person, account, bead id or incident date
  in tracked files. Keep the scar as one generic clause.
- *Configuration, not constants.* Every path, name and label is a registry key with a default
  derived from where the harness is installed.
- *Prefer no comment.* A fact belongs in a name, a guard or a test, where something runs it.
  When a comment is needed, state the rule, not the incident, in a line or two.
- *Prove the check.* A suite that finds nothing must first have found something planted.
