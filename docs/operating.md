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
verdict (`work submit --met|--contradicted|--short`). The sentinel calls dwarves for ready
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
member (path-based auto-eject, else a called `mender` or `batcher` judgement). Read and drive
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
| What is in each state, and what is stuck beyond its normal dwell? | Loom (`SPIRA_LOOM_ADDR`): `/`, `/stuck`, `/states`, `/lifecycle`, `/dwarves`, `/units`, `/rounds`, `/rounds/<repo>/<batch>` (+ `/log`), `/beads/<id>`; JSON under `/api/` (`stuck states lifecycle dwarves units ops rounds/<repo>/<batch>`) |
| Work claimed but nobody is working it | `strand report [--json]`; `strand check --dry-run` (`strand check` is the timer path) |
| Which failure classes keep recurring | `census [--with-suppressed]` |
| What the watchers saw | `watchd status`; `watchd peek [name] [--all] [--limit N]`; `watchd tail <name>` (stream from the cursor; one reader per watcher); `watchd next <name> [--timeout SECS]`; `watchd drain [name] [--all]` |
| Operator-facing questions and alerts | `mail list operator --unread`, `mail read operator [<message>]`, `mail count operator`; asks: `spira-lc list-asks [--state S]`, `spira-lc show-ask <id>` |
| The cockpit and Concierge | `cockpit.sh` builds or repairs it and attaches; `cockpit.sh status` changes nothing; `cockpit.sh resume` after a planned reboot drain; `concierge.sh status` |
| A gate | `gate status <bead>` |
| Hold a bead by hand for non-dwarf work | `hold.sh <bead-id> [--pid P \| --release]` |
| Statutes and runbooks | `rule list`, `rule show <slug>`, `rule enact <slug> "<text>" (--tool <cmd> \| --mechanism <bead> \| --tool none --reason <why>) --part <n> [--dry-run]`, `rule code [--check]`, `rule audit`, `rule retire <slug> --reason <why> [--replaced-by <slug> \| --merged-into <slug>]`; `sop list`, `sop show <slug>`, `sop write <slug> [--revive] [-\|<file>]`, `sop match [-\|<file>]`, `sop lint`, `sop retire <slug> --reason <why>`, `sop list --retired` |

Never read state from the files, logs or process lists another component owns; each state has
one owning command (see the lint rules in [development](development.md)).

## The goodput loop

The reconciler's goodput pass (`spira-reconciler-goodput.timer`, every minute) and the
lifecycle section (`spira-lifecycle-section.timer`) run on the observability plane, so a world
stop never silences them. Every section kind on `SPIRA_RECONCILER_SHADOW_KINDS` (by default
`RoundController Lifecycle Alarms`) is planned and recorded as a would-step, and nothing is
written to `intents/` for it.

| Task | Do |
|---|---|
| Read a section | `spira-lc section get <Kind>/<name>`; every section with its age: `spira-lc section list [--kind K]` |
| Read the pass record | `spira-lc section snapshot` (`pass` is the latest record); the day of history: `spira-lc pass history` |
| Arm a kind | `reconciler arm-check <Kind>`: it checks that the kind's sections are fresh, that the dead-man and heartbeat are green, and that shadow passes recorded the kind. It prints the kind's would-be steps for review and the one `spira-config set spira.reconciler_shadow_kinds ...` that takes the kind off the list and shadows every old remedy it replaces. arm-check itself writes nothing, and it exits non-zero naming each unmet precondition. spira-runtime reads the key only at start, so swap with no moment of two actuators: stop the goodput timer, run the printed command, restart spira-runtime, then start the timer. The key holds old remedies back; it does not silence the old kinds' alerts. |
| Disarm a kind | `reconciler arm-check <Kind> --disarm` prints the reverse write. `queue-mergeable` stays shadowed, because the cutover shadowed it. Run the write in the same order as arming. |
| Answer a dead-man page | the `reconciler` page means the latest pass record is older than three intervals. Check `systemctl --user status spira-reconciler-goodput.timer` and the tail of `reconciler-goodput.log` in the run directory. A standby line names the holder of `locks/reconciler` (`spira-lc lock show reconciler`). A timer the operator stopped reads as WAITING, not stale. |

## Restoring a dolt store from its off-box backup

`lc-backup.sh run` keeps the bucket at `SPIRA_LC_BACKUP_URL` laid out per database as
`<prefix>/<db>/files/<name>` (content-addressed table files, each uploaded once and never
rewritten) and `<prefix>/<db>/manifests/<UTC stamp>-<head commit>` (one per run; the newest
is the backup). The backup key can only upload, so a restore runs under your own identity:

1. **Pick the manifest.** List `<prefix>/<db>/manifests/` and take the newest key (keys sort by
   stamp). Its suffix after the `-` is the commit it restores to.
2. **Download it and the table files** into two empty directories, `m/` and `files/`. A dolt
   manifest is one colon-separated line, `5:<format>:<lock>:<root>:<gc gen>` followed by
   `<name>:<chunk count>` pairs; each `<name>` is stored as `files/<name>` or
   `files/<name>.darc`. Downloading all of `files/` is simplest; fetching only the named ones
   is enough. With the AWS CLI:

   ```
   aws s3 cp s3://<bucket>/<prefix>/<db>/manifests/<key> m/<key>
   aws s3 sync s3://<bucket>/<prefix>/<db>/files/ files/
   ```

3. **Lay down the restore source and restore it:**

   ```
   spira-lc admin-dolt-assemble --manifest m/<key> --files files --out src
   src="$PWD/src"; (cd <new data dir> && dolt backup restore "file://$src" <db>)
   ```

   `admin-dolt-assemble` refuses when the manifest names a file `files/` lacks, and prints
   `<head> <files> <bytes>`. Check `SELECT commit_hash FROM dolt_log LIMIT 1` in the restored
   database equals `<head>`.
4. **For bd, reconcile the planes** as the control store's drill prints under `skew guard`:
   the restored planning store is as old as the manifest's stamp.

The bucket's lifecycle rule may expire old `manifests/` but must never expire `files/`: a file
is uploaded once and every later manifest may still name it. `lc-backup.sh drill` proves the
same path twice a day against the local backup dir.
