# Installation and first run on a fresh box

## Prerequisites

Spira runs as systemd **user units** on Linux, so you need a systemd user manager (the
installer enables lingering for the installing user) and a login user. The authoritative tool
list is `spira/deps.toml`, which `doctor` reads:

| Tier | Tools | Notes |
|---|---|---|
| runtime (the loop cannot run) | `bd`, `dolt`, `git`, `python3`, `flock`, `tar`, `gunzip`, `duckdb` | `bd`, `dolt`, `duckdb`, `gh`, `jq`, `zstd`, `sccache`, `awscli2`, `inotify-tools`, `aerc`, `go`, `nodejs`, `openssl`, `terraform` and `tailscale` are pinned in `flake.nix` as the `spira-env` package; the installer builds it with `nix build` when `nix` is present. |
| optional (one feature off) | `claude` (the agent CLI; `SPIRA_AGENT` names the binary), `tmux` (cockpit), `gh` + `openssl` (`pr`-mode repositories), `jq`, `zstd`, `aws` (EC2 spill), `node`, `cargo` | With no `claude` the loop is armed and summons nothing; `ready.sh` warns. |
| operator | `sccache` (WebDAV backend), `inotifywait`, `aerc`, `hunk`, `go` | The installer fetches `duckdb`, `sccache`, `inotifywait` into `~/.local/bin` and copies a vendored `aerc` from the release when they are missing. |
| development | `rustup` (toolchain pinned by `rust-toolchain.toml`), `nix`, `podman`, `bd-embedded`, `terraform` | `podman` runs the test containers; `bd-embedded` is the embedded-Dolt build of `bd` used as the test-fixture engine. |

## Build and install

A release is an immutable directory under `<releases>/` with a `current` symlink; units execute
out of `current`, so a clone is never the running copy.

```sh
make dist      # cargo build --release --workspace, then spira/build-tarball.sh build --workspace .
               # -> ./spira-<UTC timestamp>.tar.gz
```

Write an answers file (`key = value`, `#` comments). These are required; any other key
registered under `[spira]` may also be given and is validated like the rest:

```
instance  = prod                        # name of this installation
id_prefix = sp                          # prefix of your bead ids, no hyphen
home_repo = <repo>                      # the repository this harness works on
db        = <home>/.local/share/spira/db
run       = <home>/.local/run/spira
releases  = <home>/.local/share/spira/releases
dolt_data = <home>/.local/share/spira/dolt
# optional: the home repository's registry row
home_repo_path = <path to checkout>
home_repo_land = push                   # push | pr | queue.local   (default push)
home_repo_base = origin/main
```

```sh
target/release/release install-tarball spira-<ts>.tar.gz --answers answers.txt
export SPIRA_TOML=<path the command printed>   # default: ${XDG_CONFIG_HOME:-~/.config}/spira/spira.toml
<releases>/current/bin/spira-install --dry-run # what it would do
<releases>/current/bin/spira-install           # then for real
```

`release install-tarball` unpacks beside existing releases, swaps `current` atomically and, if
no `spira.toml` exists, writes one from the answers (every unanswered key gets its registered
default; an existing file is validated and never overwritten). With no answers file and a
terminal it prompts; with neither it refuses and names the missing inputs.

`spira-install [<instance>] [--dry-run] [--ephemeral] [--laptop] [--skip-build]
[--no-session-hook] [--system-user] [--populate-lifecycle]` runs, in order: dependencies,
`doctor` preflight, conflict checks, config, same-user lifecycle credential, build, database,
units, lifecycle store, desired state, hooks, cockpit, verify (ending with `ready.sh`).
`--system-user` and `--populate-lifecycle` are standalone single-phase modes. Exit codes: 0
ready, 1 preflight refused, 2 a phase failed, 3 installed but not ready, 5 a conflict (another
copy's units, a live aeon, the gate lock, an instance/run-dir or Dolt-port collision; the
message names the remedy and the override `SPIRA_INSTALL_CONFLICT_CONSIDERED=1`). It refuses a
beads database inside a git checkout, or one whose own repository has a remote.

To re-render and re-enable units without a full install: `units-install [<instance>]
[--diff | --render | --laptop]`; to install missing units without restarting anything:
`unit-ensure [--diff]`. After a release activates, `spira/first-run.sh --range <A>..<B> --repo
<path>` starts each newly shipped oneshot unit once and files a bead for any that fails.

Put `<releases>/current/bin` and `<releases>/current/spira` on your `PATH`; units and aeons get
that prefix from their launchers.

## Configuration

**One source.** The configuration is the file or layered spec named by `SPIRA_TOML` and nothing
else: no search of the checkout, XDG or `/etc`, and no default for a key that is not declared.
Unset or missing, `spira-config` and `conf.sh` refuse by name. (`install`, `doctor` and
`ready.sh` adopt `~/.config/spira/spira.toml` when `SPIRA_TOML` is unset and the file exists,
because they run before anything exports it.) `SPIRA_TOML` may be `base:override[:...]`; every
layer must exist. A value resolves environment variable first, then `spira.toml`, then the
registered default; the environment is the seam test fixtures drive.

**Configuration goes through the CLI only.** Only the `spira-config` crate may open, parse or
write `spira.toml` or the repo map; the `config-fence` lint refuses any other code that does.

| Command | Does |
|---|---|
| `spira-config validate [file]` | exit 1 naming the TOML path of the first error; also refuses a `[spira]` table with no `id_prefix` |
| `spira-config get <dotted.path> [file]` | one value, e.g. `spira.max_aeons`, `repo.<name>.mode`, `persona.builder.model` |
| `spira-config set <dotted.path> <value> <file>` / `unset <dotted.path> <file>` | edit one value in place |
| `spira-config export --sh [file]` | `[spira]` as quoted `KEY=value` lines |
| `spira-config resolve --sh\|--sh-all [--conf-d DIR] [file]` | env > toml > default, as shell assignments |
| `spira-config locate` | the file in force, or the refusal naming why |
| `spira-config init [--out F] [--answers F] [--<key> V]...` | write a first `spira.toml` (never overwrites) |
| `spira-config schema` | the JSON Schema `spira.toml` is validated against |
| `spira-config repo names\|all\|field\|add\|check ...` | the repository registry |
| `spira-config fayth names\|get\|roster\|task\|lane\|partitions\|for-labels\|model` | the persona registry |

On an installed (read-only) release, callers that regenerate config resolve their write target
through `spira-config writeback <candidate>`, which redirects away from the release unless
`SPIRA_CONFIG_WRITE=1`. `aeons` (below) is the friendly door for the two pool-size keys.

**The key registry** is `spira/conf.d/<KEY>`, one file per key: `TYPE=` (`string|u32|u64|bool|list|onoff|czar_stage`),
optional `MAX=` and `SCHEMA=`, `GROUP=`, `DOC=`, and a default block holding the default as one
`: "${KEY:=...}"` statement. `SPIRA_FOO` appears in `spira.toml` as `spira.foo`, and
`COCKPIT_FOO` as `spira.cockpit_foo`. `spira/conf.toml.d/` holds keys that exist only in the
typed config. `spira/conf.d.*.generated.sh` are generated by `spira/conf-gen.sh` and never
edited. Adding a key means adding its file, declaring it under `[spira]` in
`spira-config/tests/fixtures/complete.toml`, listing it in
`spira-config/schema/spira-key-history.txt`, and, when a release adds or drops a key,
declaring it in `spira/config-delta.toml` (applied at activation). Unknown keys are a parse
error.

**Two keys are deliberately not configurable: `SPIRA_HOME` and `SPIRA_REPO`.** Where the
harness is, is a fact about where `conf.sh` sits. The landing gate extracts a branch to a
scratch tree and runs that tree's suites; a config that could point them at the installed copy
would make the gate test the code already in force.

`spira.toml` has four top-level tables:

| Table | Content |
|---|---|
| `[spira]` | the registered keys (`id_prefix` is required) |
| `[repo.<name>]` | `path`, `mode`, `base`, `format`, `lanes`, `forge`, `gate_mode` (`suites` default, or `unit`) |
| `[persona.<name>]` | `model`, `tools`, `labels`, `lane`, `lease`, `system_prompt`, `escalation_model` |
| `[publish.<name>]` | `remote` (`none` = publishes nowhere) and `branch`, for `queue.local` repositories |

**Repositories.** The registry is the pipe-delimited file named by `spira.repo_map` when it
exists (`name | path | mode | base | format | gate | lanes`), else the `[repo.*]` tables.
Register one with `spira-config repo add <name> --path P --queue local|forge [--remote URL]
[--land-ref R] [--format CMD] [--gate CMD]` and audit with `spira-config repo check
<name>|--all`. A repository carries its own gate in the tree under test (`gate.steps`) and, for
repositories other than the harness, its test entry in `.spira/testenv.toml`.

**Personas.** `spira/chamber/<name>.fayth` and `<name>.md` ([concepts](concepts.md)). The live model per
persona is `persona.<name>.model`; the `FAYTH_MODEL` line only seeds it. The aeon pool size is
`spira.max_aeons`.

## Starting and stopping the world

`world` (also installed as `world.sh`) stops and starts the work as a whole, by the
`Plane=work|observability|maintenance` each unit declares in its `[X-Spira]` section.

| Command | Effect |
|---|---|
| `world status` | halted / draining / running, essential timers, round-VM state |
| `world stop [--why "..."] [--work\|--observability\|--maintenance\|--all\|--hard] [--round-drain] [--round-drain-timeout SECS] [--grace SECS]` | halt planes (default `work`); `--hard` = work + observability; live aeons are stopped through `slay`, never killed |
| `world start [--work\|--observability\|--maintenance\|--all]` | default starts work + observability |
| `world drain [--reboot] [--timeout SECS \| --deadline SECS]` | stop summoning new aeons only; loop and landing keep running; `--deadline` slays what remains at the end |
| `world resume` | lift a drain |

`world` never stops Dolt, the cockpit or the Concierge. A halt names any round in flight;
`--round-drain` waits for it first.

Pool size: `aeons` (show), `aeons set <n>` (fleet ceiling across all personas), `aeons unset`,
`aeons pool <n>` (the task pool, `SPIRA_MAX_AEONS`). Stop one aeon: `aeon stop <bead> [--why
<text>]` or `slay --bead <id> [--why "<text>"] [--keep-work] [--close "<reason>" | --reopen]`.

## Checking health

- `doctor`: read-only; runs every check and exits 1 on any FAIL. It covers release tools,
  config, the store, the compilation cache, round-VM mirror, systemd units, the cockpit, the
  operator channel, the Concierge, git hooks and `spira-env`.
- `ready.sh`: the postflight, "can this installation receive work right now?" (sentinel,
  timers, world, database, ready work, loom, agent, cockpit). Exit 1 on any FAIL or unknown.
- `world status`, `release status [--json]`, and the pages in [operating](operating.md).
