# Reinstalling an instance from a release tarball

Install a release onto a box that already runs an older Spira, or onto a clean one. The same
steps serve both: step 2 is skipped on a clean box. The old instance is not migrated; it is
replaced. Nothing is restored from it apart from an optional safety archive.

Each step gives the commands, then **Success**, then **If it fails**. Angle brackets are
placeholders. Never rename the tarball: `release install-tarball` refuses any name that is not
`spira-<timestamp>.tar.gz`.

## What proves this runbook

Steps 3–7 are the human form of acceptance phase A (`release/src/acceptance/phases.rs`): in a
clean container, as an unprivileged user, a release tarball is unpacked, a config is generated
from an answers file, `spira-install` runs, `ready.sh` passes, one bead is filed and walked
`READY → WORKING → SUBMITTED → CERTIFIED → LANDED`, and `uninstall.sh` leaves nothing behind.
Re-run it against any release with `release acceptance <tag> --scratch-repo <path>` (`--help` lists the rest). Acceptance uses a stub agent and a
local scratch remote, so a **Not rehearsed** note marks each place a real box differs.

## 1. Before you start

- Tools on PATH: `git`, `dolt`, `bd`, `python3`, `tmux`, `curl`, `sha256sum`, and a working
  `systemctl --user` (over ssh, `XDG_RUNTIME_DIR` must be set). Config generation refuses with
  `no bd on PATH` otherwise.
- Egress to the forge host: `spira-install` downloads missing helper tools in phase −1.
- The agent CLI is installed and logged in. *Not rehearsed:* without it the loop arms and
  summons nothing, and the Verify bead spends real tokens.
- A checkout of the repository this instance works on, whose remote accepts pushes **without
  a prompt**. Pushes come from systemd user units, which do not see an interactive ssh-agent.
  *Not rehearsed:* acceptance pushes to a local bare repository.
- Dolt's port (default 3307) is free once the old instance is stopped.
- Disk for the database and releases, and time: allow an hour.
- Record the session: `script -a -f <dir>/session-$(date -u +%Y%m%dT%H%M%SZ).log`.
- No step prints a credential. Never `cat` files under `~/.config/spira/` named `*credential*`.

## 2. Optional: stop and wipe the old instance

The installer refuses a unit it did not render (phase 0.5, foreign harness), refuses a second
Dolt on its port, and **adopts** any `~/.config/spira/spira.toml` it finds.

**2a. Record where the old instance keeps things**, from what is running:

```bash
mkdir -p <work> && cd <work>
systemctl --user list-unit-files --no-legend --plain \
  'spira-*' 'dolt-*' 'lc-serve*' 'concierge*' 'cockpit*' 'beads-push*' | awk '{print $1}' | sort -u > old-units.txt
systemctl --user cat 'spira-sentinel-*.service' | grep -oE 'SPIRA_(DB|RUN|DOLT_DATA|RELEASES)=[^ ]*' | sort -u
```

Write the answers as literal paths in `<work>/old.sh` (`OLD_DB`, `OLD_DOLT`, `OLD_RUN`,
`OLD_RELEASES`, `OLD_RELEASE`) and `. <work>/old.sh`.

**2b. Stop everything, the database included:**

```bash
systemctl --user stop $(cat old-units.txt); sleep 3
systemctl --user list-units --state=active --no-legend --plain | grep -E '^(spira-|dolt-|lc-serve|concierge|cockpit)'
pgrep -af 'dolt sql-server'; ss -ltn | grep ':3307\b'
```

**Success:** the last three commands print nothing. An aeon mid-bead is abandoned.
**If it fails:** a unit that returns has a `Restart=` policy or a timer: `systemctl --user
disable --now <unit>`. Stop a stray `dolt sql-server` by name, never by a PID derived from a
parent PID.

**2c. One safety archive**, cold because the database is stopped:

```bash
tar -C / -czf <work>/old-instance.tar.gz "${OLD_DB#/}" "${OLD_DOLT#/}" "${OLD_RUN#/}" "${OLD_RELEASE#/}" \
  "${HOME#/}/.config/spira" "${HOME#/}/.config/systemd/user"
tar -tzf <work>/old-instance.tar.gz >/dev/null && echo readable
```

Copy it off the box if it must outlive it. **This is the last point where nothing is lost**;
to back out, `systemctl --user start $(cat old-units.txt)`.

**2d. Uninstall and wipe.** Prefer the old release's own `uninstall.sh --yes`; fall back to
`systemctl --user disable --now` and removing each unit file and `.d` directory, then:

```bash
systemctl --user daemon-reload
rm -rf "$OLD_DB" "$OLD_DOLT" "$OLD_RUN" "$OLD_RELEASES" ~/.config/spira
systemctl --user list-unit-files --no-legend --plain 'spira-*' 'dolt-beads*' 'lc-serve*'
```

Remove old Spira lines from the login profile: an exported `SPIRA_HOME`, `SPIRA_TOML` or
`SPIRA_REPO`, or a PATH entry into an old release, overrides the new one.

**Success:** the unit listing is empty and `ls ~/.config/spira` fails.

## 3. Fetch and verify the tarball

```bash
curl -fLO <url>/spira-<ts>.tar.gz && curl -fLO <url>/spira-<ts>.tar.gz.sha256
sha256sum -c spira-<ts>.tar.gz.sha256
mkdir -p stage && tar -xzf spira-<ts>.tar.gz -C stage
STAGE=$PWD/stage/spira-<ts>; head -1 "$STAGE/MANIFEST"
```

**Success:** `OK`, then the MANIFEST's first line names the commit the release was cut from.
The staged copy is used only for its `bin/release`, before anything is installed.
**If it fails:** a sha mismatch means fetch again; a second mismatch means stop.

## 4. Write the answers file

`release install-tarball --answers` runs `spira-config init`, which writes
`~/.config/spira/spira.toml` from a few values. Every other key takes its registered default.
An existing config is validated and used as is, never overwritten.

```ini
instance       = prod
id_prefix      = sp
home_repo      = <repo-name>
home_repo_path = /abs/path/to/its/checkout
home_repo_land = push
home_repo_base = origin/main
db             = /abs/path/db
dolt_data      = /abs/path/dolt
run            = /abs/path/run
releases       = /abs/path/spira-releases
```

- Use absolute paths; no `~` or `$HOME`.
- `home_repo` becomes the `repo:` label. `home_repo_land` is `push`, `pr` or `queue.local`;
  `push` is the simplest proof. `home_repo_base` is the ref work lands on.
- `db` and `dolt_data` sit **outside any git checkout** (install refuses otherwise) and are
  best kept as siblings.
- Keep `instance = prod` and `id_prefix = sp`: the installer reads the instance from
  `SPIRA_INSTANCE` or its first argument, and a fresh database is created with prefix `sp`.
- To keep an existing beads database instead of starting empty, point `db` and `dolt_data` at
  it and keep its lifecycle credentials with it; install adopts it and gives every bead a
  lifecycle row once. *Not rehearsed.*

## 5. Install

Export these **before** `spira-install`: phase −1 runs before it reads any config.

```bash
R=<releases>; mkdir -p "$R"
export SPIRA_TOML="$HOME/.config/spira/spira.toml" SPIRA_RELEASES="$R" SPIRA_RUN=<run> SPIRA_RELEASE="$R/current"
export PATH="$R/current/bin:$R/current/spira:$HOME/.local/bin:$PATH"
```

**5a. Unpack and write the config:**

```bash
"$STAGE/bin/release" install-tarball --skip-restart <work>/spira-<ts>.tar.gz --answers <work>/answers
readlink "$R/current"; hash -r; command -v spira-install ready.sh bead.sh
```

**Success:** `wrote …/spira.toml from the operator's answers`, then `spira-<ts> installed`;
`current` points at `spira-<ts>` and all three tools resolve under it.
**If it fails:** nothing is installed until the config is written, so fix and re-run.
`missing required input(s)` names a blank or misspelled key; `no bd on PATH` means install
beads or add `bd = /abs/path/to/bd`; a validation error from an existing `spira.toml` means
step 2 was not done.

**5b. Services, database and lifecycle store:**

```bash
spira-install --skip-build 2>&1 | tee <work>/install.log; echo "rc=${PIPESTATUS[0]}"
```

**Success:** it ends `install: done — Spira is ready.` with `rc=0`. The log passes, in order,
config, database (`initialising database`, `seeding statutes`), units (`dolt-beads.service
listening`), lifecycle store (credentials written 0600) and
`lifecycle population: 0 bead(s) in the database, 0 row(s) created` on an empty database,
then hooks. `spira-install` is idempotent: fix the cause and re-run.

| rc / message | Cause | Do |
|---|---|---|
| `1`, `preflight failed` | a `doctor` FAIL | read its FAIL lines; with an operator present a missing `tmux` is a FAIL |
| `5`, `CONFLICT` | a leftover old unit, a live aeon, a held gate lock, or a Dolt on the port | follow the printed `remedy:`; usually step 2 was incomplete; do not use the override |
| `2`, `phase config failed` | `SPIRA_TOML` unexported or naming no file | re-export step 5's block |
| `2`, `REFUSING database … inside a git checkout` | `db` is under a git repo | choose another path, remove `spira.toml`, redo 5a |
| `2`, `root accepts neither the password` | a Dolt with a hand-set root password survived | wipe its data, or supply `SPIRA_LC_ADMIN_USER`/`SPIRA_LC_ADMIN_PASSWORD` without echoing |
| `2`, `lc-serve.service is not active` | lifecycle service failed to start | `journalctl --user -u lc-serve.service -n 50` |
| `3`, `installed but NOT ready` | `ready.sh` found a FAIL | step 7; *not rehearsed:* inside tmux the cockpit row can FAIL until its collector writes a snapshot, so wait two minutes |
| `1`, network errors in phase −1 | a download failed | fix egress and re-run |

## 6. Launchers

Units get their environment rendered into them; anything you start yourself has only what
your profile gives it. Add to the file your login shell reads, with `<RELEASES>` literal:

```bash
export SPIRA_TOML="$HOME/.config/spira/spira.toml"
export SPIRA_RELEASE="<RELEASES>/current"
case ":$PATH:" in *":$SPIRA_RELEASE/bin:"*) ;; *) PATH="$SPIRA_RELEASE/bin:$SPIRA_RELEASE/spira:$HOME/.local/bin:$PATH" ;; esac
export PATH
```

Check it from a bare environment, then check the client hooks as the client runs them:

```bash
env -i HOME="$HOME" TERM="$TERM" bash -lc 'echo "TOML=$SPIRA_TOML"; command -v spira-config mail world.sh; "$SPIRA_RELEASE/cockpit.sh" status; echo "rc=$?"'
release session-hook status
```

**Success:** `TOML=` shows the path, the tools resolve under `<RELEASES>/current`, `rc` is 0 or
1 (a state was reported), and the hook status shows no `MISSING`, `DUP` or `STALE`. A `DUP` or
`STALE` clears with `release session-hook install`. Hook commands must go through `current`,
never a release directory, so they survive an upgrade.

Mail client: create `~/.config/aerc/accounts.conf` from `$SPIRA_RELEASE/aerc/accounts.conf`
(substituting the Maildir and `~/.local/bin/spira-sendmail` paths), then
`~/.local/bin/spira-sendmail --check` and `mail-health.sh` (exit 0 or 1). A missing
`spira-sendmail` means `SPIRA_RELEASES`/`SPIRA_TOML` were unexported during 5b: export them and
re-run `spira-install --skip-build`.

## 7. Ready, then verify with one bead

```bash
ready.sh; echo "ready rc=$?"
```

**Success:** it ends `armed: loop is ready to receive work.` with `rc=0`. On an empty database
the ready-work row is a WARN, as is the agent row if the CLI is absent. `?` means "could not
read", never a pass.

File one bead the way a human does, so the lifecycle row is written as part of filing:

```bash
git -C <home_repo_path> fetch origin; BASE=$(git -C <home_repo_path> rev-parse <home_repo_base>)
printf 'Install proof. Commit a file named install-proof.txt at the repository root containing one line: ok\n' > <work>/proof.md
bead.sh file "install proof: add install-proof.txt" --for builder --repo <home_repo> --body-file <work>/proof.md
ID=<the id it printed>
spira-lc history "$ID"                                   # READY
spira-claim fayth-ready builder --json | grep -c "$ID"   # 1 = claimable
systemctl --user start spira-sentinel-prod.service
git -C <home_repo_path> show-ref "refs/heads/spira/$ID"  # the aeon's branch appears
spira-lc history "$ID" | tee <work>/proof-history.json
git -C <home_repo_path> fetch origin; git -C <home_repo_path> log --format='%H %s' "$BASE..<home_repo_base>" | grep "$ID"
```

**What proves it:** the bead is claimable; the branch `spira/<ID>` carries a commit naming
`<ID>`; the history passes `READY → WORKING → SUBMITTED → CERTIFIED → LANDED` (for
`queue.local`, `IN_DELIVERY → QUEUED → BATCHED` precede `LANDED`; `LANDED` is recorded one
audit pass after the push); and a commit naming `<ID>` is on the base ref after `$BASE`.

**If it fails:** `repo:<x> is not in the repo map` means `--repo` must equal `home_repo`
(`spira-config repo names`). No branch after about five minutes: check the `ready.sh` agent
row, `journalctl --user -u spira-sentinel-prod.service -n 60`, and that the agent is logged in.
Stuck at `SUBMITTED`: `journalctl --user -u spira-gate-worker-prod.service -n 80`. `CERTIFIED`
without `LANDED` is usually the push; read `spira-landing-pass-prod.service` for an auth
error. In every case leave the world running and do not hand-edit bead state.

## 8. Rollback

```bash
uninstall.sh --yes    # units, linger stamp, session hook, alert drop-ins; keeps config, run dir and database
systemctl --user list-unit-files --no-legend --plain | grep '^spira-' || echo "no spira units"
```

Add `--purge` (config and run dir) and `--purge-database` (asks for the bead count back) to
remove data too. To restore the old instance from the 2c archive:

```bash
rm -rf ~/.config/spira <db> <dolt_data> <run> <releases>
tar -C / -xzf <work>/old-instance.tar.gz
systemctl --user daemon-reload
systemctl --user enable --now $(grep -E '\.(timer|service)$' <work>/old-units.txt | grep -v aeon)
```

Then restore the old profile lines. `uninstall.sh` tolerates missing pieces and exits 0 on a
second run; stray units it reports come from an older harness and are disabled and deleted by
name.
