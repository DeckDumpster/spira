# Operating it day to day

## Filing work

All work enters as a bead through the contract; never `bd create` directly.

```sh
bead.sh contract                    # legal personas, repos and kinds, read from source
bead.sh file "<title>" --for builder --repo <name> [--priority N] [--body-file F] [--parent <id>] [--stack-after <id>] [--target "<metric> <op> <value>"] [--express] [--json]
bead.sh file "<title>" --kind <kind> [--repo <name>] ...
bead.sh amend <id> [--note "<text>"] [--body-file F --replace | --append-body F] [--priority N] [--express]
bead.sh label add|remove <id> <label>
bead.sh dep add <id> <depends-on-id> [--type <type>]
bead.sh dep remove <id> <depends-on-id>
bead.sh drop <id> --reason <text>
bead.sh lint [--all|<id>...]
```

`--for <persona>` derives the partition labels from the persona's own predicate, so the filing
tool and the claim predicate cannot disagree. Kinds without a persona (`event`, `escalation`,
`proposal`, `insight`) are created closed and are deliberately unclaimable. A `--repo` not in
the registry is refused. A bead whose description carries a `Target:` line closes only with a
verdict (`work submit --met|--contradicted|--short`). The sentinel summons aeons for ready
beads; `sentinel --report` prints the open plan beads without changing anything.

## Queue modes

Each repository has a mode (`[repo.<name>].mode`, or the registry file's third column):

| Mode | What the landing pass does with a finished branch |
|---|---|
| `pr` | rebase, confine, force-push, open or refresh the pull request; the PR's CI is the gate (`landing-pass --pass`, its own timer) |
| `push` (default) | rebase, confine, gate, merge in a private landing worktree, push to the base |
| `hold` | rebase, confine, gate, note "gated and held"; never advances the base. `held.sh [--summary] [<repo>]` lists; `held.sh --merge <bead-id>` fast-forwards after confirmation |
| `queue.forge` (alias `queue`) | certify the tip once; the forge's merge queue lands rounds; `queue step` settles open rounds |
| `queue.local` | same certification; rounds are cut by `batcher`, landed with `queue land-local`, published to the forge target with `queue publish` |

A gate returns `PASS`, `FAIL` (only this reopens and charges the branch), `BASE_FAIL` (the base
itself is red) or `NO_VERDICT` (the harness could not judge); nothing lands on the last three.
Inspect or stop a gate with `gate status <bead>` and `gate cancel <bead> [--why <text>]`.
`landing-pass halt [--reason T | --reason-file F|-] [--dry-run]` halts a running landing pass.
Switch a repository between local and forge queues with `queue to-forge [<repo>]` /
`queue to-local [<repo>]`; undo a local landing with `queue rollback-local [<repo>]`.

Read a bead's state with `spira-lc show <bead-id>` and
`spira-lc list [--live] [--state S[,S...]] [--repo NAME] [--hold poison|ask|wait|manual]`. A
closed bead is not a landed bead.

## Rounds

For queue repositories `batcher rounds-local` (every 30 s) cuts, tests and lands rounds, and
`queue step --all` (every 2 min) settles open batches. A round that goes red is attributed to a
member (path-based auto-eject, else a summoned `mender` or `batcher` judgement). Read and drive
rounds with:

Switch a repository's mode with `queue mode-switch <repo> <queue.forge|queue.local>`: it settles
the forge batch first and refuses while one is unsettled. A forge batch left behind by any other
route is settled by `queue verdict` whatever the mode, and `rounds-local` ignores forge
`open`/`prepared` records for a queue.local repo.

```sh
queue round status [<repo>]
queue step <repo>
queue eject <id> [--reason <text>] [--harness-fault] [--dry-run] [<repo>]
queue abandon [<repo>] --reason <text> [--dry-run]
queue open-batch [<repo>] [--members <ids>] [--skip-pregate] [--dry-run]
```

`queue` with no arguments prints the full verb set.

## Releases

| Command | Does |
|---|---|
| `release build <commit> [--repo R] [--target-dir T \| --bin-dir D]` | build `<releases>/<sha>`; prints the sha |
| `release verify <sha> [--no-pre-activate]` | exit 1 listing problems |
| `release activate <sha> [--hotfix <reason>] [--repo R] [--landed-ref REF] [--settle SECS] [--drain-wait SECS]` | switch `current`, re-render and restart changed units, prune |
| `release rollback [--repo R] [--settle SECS] [--drain-wait SECS]` | back to the previous release |
| `release status [--json]` / `release prune [--keep N]` | state; remove old releases |
| `release install-tarball <tarball> ...` | the packaged-tarball pipeline ([install](install.md)) |

Every subcommand also takes `--releases D` and `--run D`. A release is never deployed from a
worktree: merging does not deploy, activation does.

## When something is stuck

| Question | Look here |
|---|---|
| What is in each state, and what is stuck beyond its normal dwell? | Loom (`SPIRA_LOOM_ADDR`): `/`, `/stuck`, `/states`, `/lifecycle`, `/aeons`, `/units`, `/rounds`, `/rounds/<repo>/<batch>` (+ `/log`), `/beads/<id>`; JSON under `/api/` (`stuck states lifecycle aeons units ops rounds/<repo>/<batch>`) |
| Work claimed but nobody is working it | `strand report [--json]`; `strand check --dry-run` (`strand check` is the timer path) |
| Which failure classes keep recurring | `census [--with-suppressed]` |
| What the watchers saw | `watchd status`; `watchd peek [name] [--all] [--limit N]`; `watchd tail <name>` (stream from the cursor; one reader per watcher); `watchd next <name> [--timeout SECS]`; `watchd drain [name] [--all]` |
| Operator-facing questions and alerts | `mail list operator --unread`, `mail read operator [<message>]`, `mail count operator`; asks: `spira-lc list-asks [--state S]`, `spira-lc show-ask <id>` |
| The cockpit and Concierge | `cockpit.sh` builds or repairs it and attaches; `cockpit.sh status` changes nothing; `cockpit.sh resume` after a planned reboot drain; `concierge.sh status` |
| A gate | `gate status <bead>` |
| Hold a bead by hand for non-aeon work | `hold.sh <bead-id> [--pid P \| --release]` |
| Statutes and runbooks | `rule list`, `rule show <slug>`, `rule enact <slug> "<text>" (--tool <cmd> \| --mechanism <bead> \| --tool none --reason <why>) [--dry-run]`, `rule audit`, `rule retire <slug>`; `sop list`, `sop show <slug>`, `sop write <slug> [-\|<file>]`, `sop match [-\|<file>]`, `sop lint`, `sop retire <slug>` |

Never read state from the files, logs or process lists another component owns; each state has
one owning command (see the lint rules in [development](development.md)).
