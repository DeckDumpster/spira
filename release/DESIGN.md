# release — design

The release producer (sp-m3ipc, work item 1 of the epic "the running system is a release",
brain `wiki/projects/spira/designs/runtime-is-a-release-2026-09-29.md`). It replaces, for
local use, the Makefile's `install` target, `spira/activate.sh` and `spira/build-tarball.sh`.

**sp-jsnbm** (wave 2 of the rewrite programme) extends it to the public (GitHub tag/tarball)
pipeline: `install-tarball`, `stage` and `canary` move `spira/activate.sh`, `spira/stage.sh`
and `spira/canary.sh` into this crate, so the GitHub acceptance workflow and `install.sh` call
the same binary the local path does. `spira/build-tarball.sh` stays bash (it produces the
tarball `install-tarball` consumes; a separate concern, untouched here — DESIGN.md "Callers").

## Intent

*"In the running system, there should be ABSOLUTELY ZERO ambiguity about which binaries to
use"*, and using the system is decoupled from building it (the operator, in the approved
design).

The running system executes exactly one thing: a directory `spira-releases/<sha>/` that is
immutable, read-only and named by the commit it was built from. This crate is the only thing
that makes such a directory, checks one, and switches the running system onto one. Nothing it
does ever points the running system at a checkout, a worktree or a `target/` directory.

## Contract

```
release build <commit> [--repo R] [--target-dir T | --bin-dir D]  build spira-releases/<sha>, print <sha>
release verify <sha> [--no-pre-activate]                MANIFEST, unit binaries, pre-activate
release activate <sha> [--hotfix "<reason>"] [--repo R] [--landed-ref REF] [--settle S]
release rollback [--settle S]                           activate the previous release
release prune [--keep N]                                keep the newest N releases
release status                                          current, previous, RUNNING UNLANDED, ALERT
release install-tarball <tarball> [--dry-run] [--settle S]   the public pipeline's own install
release stage up [ROOT] | down <ROOT>                   an isolated Spira for testing
release canary [--stage ROOT] [--deadline S]            end-to-end pipeline canary on a stage
release canary-worker                                   the stage's synthetic aeon (internal)
release acceptance <tag> --scratch-repo P [...]         release acceptance on a clean machine (DESIGN.md "acceptance")
release session-hook install|status|uninstall|prune S  the client's SessionStart hook + statusLine (DESIGN.md "session-hook")
release intake install|status|uninstall                 wire systemd alert units into incident intake (DESIGN.md "intake")
```

Every subcommand also takes `--releases D` (else `$SPIRA_RELEASES`, else `spira.releases`,
else `<spira.workspaces>/spira-releases`) and `--run D` (else `$SPIRA_RUN`, else
`spira.run`). `spira.toml` is read from `$SPIRA_TOML`, else
`$XDG_CONFIG_HOME/spira/spira.toml` (`~/.config`) — never from a checkout. A root nothing
resolves is a refusal naming the key; there is no built-in path.

Exit `0` success; `1` refused or failed, with the reason on stderr; `2` usage.

### build

1. Resolve `<commit>` in `--repo` (else `$SPIRA_REPO`, else the current directory; a bare
   repository works) to a full sha. If `spira-releases/<sha>` already exists it is not
   rebuilt: releases are immutable, so an existing one is either right or a verify failure.
2. `git archive <sha>` into `spira-releases/.stage-<sha>-<pid>/` (the same filesystem, so
   step 7 is a `rename(2)`).
3. `cargo build --release --workspace --locked` in the stage, with `CARGO_TARGET_DIR` at
   `--target-dir` (else `$SPIRA_RUN/release/target`, a cache that is not part of any
   release; else a throw-away `.target-<pid>` beside the stage). **With `--bin-dir D`
   there is no cargo build:** the binaries are taken from `D`, a tested build of this commit
   the caller vouches for — `queue land-local` passes the round's own `target/release`
   (law-deploy-the-tested-artifacts: a rebuild is a different artifact from the one that
   passed; sp-gkfg1). `--bin-dir` and `--target-dir` exclude each other.
4. Copy every workspace `[[bin]]` target (from cargo's output or `--bin-dir`) into `bin/`;
   anything else in that directory is not shipped. The list is read from the commit's own
   `Cargo.toml` members ([`workspace::expected_bins`] — the one reader; `queue`'s deploy
   calls the same function). A declared binary the build did not produce is a refusal.
5. **Name clashes:** an executable directly in `bin/` or `spira/` whose name is also a
   command in `/usr/local/bin`, `/usr/bin` or `/bin` is a refusal. Those are the directories
   every launcher's PATH ends with, and the release comes first on it, so such a file would
   silently shadow a system command.
6. Write `MANIFEST`, then `chmod -R a-w` the stage.
7. Rename the stage to `spira-releases/<sha>`. Any failure removes the stage; nothing
   partial is ever named by a sha.

### MANIFEST

Plain text, one record per line, sorted by path after the header:

```
commit <40-hex sha>
built <RFC 3339 UTC>
repo <home repo name>            (only when spira.home_repo is set; conf.sh reads it)
<path> <sha256>                  every regular file, bin/ included
<path> -> <link target>          every symlink
```

The `<path> <sha256>` shape is the one `spira/self-test.sh` and the old `make install`
already read, so a release made here passes the release's own self-test. `MANIFEST` does not
list itself. A tracked file whose path is a header keyword is a build refusal.

### verify

Passes only if all hold, and reports every failure, not just the first:

* `MANIFEST`'s `commit` is the directory's name;
* every listed file exists and hashes to its entry, every listed symlink points where it
  says, and no file or symlink exists that is not listed;
* nothing in the release is writable;
* the name-clash rule still holds;
* **every binary a unit template names exists:** in each `systemd/*` template, every path
  under the release that an `Exec*=` line names (after the release's own placeholders are
  filled) exists, and the program each one runs is executable;
* the release's own `spira/pre-activate.sh <release>` passes (skip with
  `--no-pre-activate`; a release that has none is a failure).

**pre-activate's environment (sp-vrn3v).** `pre-activate.sh`'s `deps` check resolves every
release-tier binary with `command -v`, so it runs with `SPIRA_RELEASE` and `PATH` set from
the release **under verification** — `<rel>/bin:<rel>/spira:<system dirs><tail>`
(`release_path_with_tail`, the box's own tool tail from `Config::path_tail`) — never
inherited from the caller. Before this, the child ran with whatever `PATH` the caller's
shell had; at cutover that was the old checkout's `spira-config`, which lacked
`path-tail`, and verify failed judging binaries that were never the release's own. Every
other inherited variable is left alone: only `SPIRA_RELEASE` and `PATH` are ever ambiguous
about which release they name.

### activate

`activate <sha>` switches the running system onto a verified release:

1. Check the MANIFEST (the file half of verify: cheap, and the one thing that makes a
   switch safe).
2. **Hotfix rule** (below). A refusal changes nothing.
3. Render every installed unit that comes from one of the release's templates, against
   `spira-releases/<sha>` — never `current`, so a unit states exactly what it runs. Any
   render failure is a refusal before anything is touched.
4. Install the rendered files that differ (write `.new`, rename).
5. Swap `current` atomically (a symlink written beside it, renamed over it).
6. `daemon-reload`.
7. Restart each **service whose `Exec*=` lines changed, that is active, and is not a
   oneshot**. An active oneshot finishes its run from the old release, which is still on
   disk and still immutable, and its next start reads the new unit file. Inactive units are
   not started: that is their timer's job.
8. Check each restarted unit came up (after `--settle` seconds, default 3): `ActiveState`
   is `active`. If any did not, **roll back**: restore every unit file this activation
   changed, point `current` back where it was, daemon-reload, restart the same units, and
   exit 1 naming the unit that failed.
9. Record the activation in `$SPIRA_RUN/release/history`.

**Which units.** Every regular file in the systemd user unit directory
(`$SPIRA_UNIT_DIR`, else `$XDG_CONFIG_HOME/systemd/user`) that maps back to a template in
the new release's `systemd/`: a shared unit by its own name (`concierge.service`); a
per-instance unit `spira-<x>-<instance>.<service|timer>` to `spira-<x>.<service|timer>`; a
watcher `spira-watch-<name>-<instance>.service` to `spira-watch@.service`. Masked units
(symlinks) and units with no template are not touched. Installing a unit for the first time
and removing a retired one remain `systemd/install.sh`'s job; activation re-renders what
is installed. (Decided in sp-gkfg1, queue/DESIGN.md §8 D13: which units a host runs is host
policy with one reader, units.sh; a unit a release adds or retires is a design change whose
cutover steps run install.sh.)

**Render.** `systemd/render.py`'s rules, in Rust: every `@KEY@` is replaced; `%i` becomes
the watcher name; a `spira-*.timer`'s `Unit=spira-<x>.service` gains the instance suffix;
the output is compared and written with exactly one trailing newline (what `install.sh` and
`unit-ensure.sh` write). Release keys come from the release: `SPIRA_HOME` and `SPIRA_PROD`
are `<rel>/spira`, `SPIRA_PROD_ROOT`, `SPIRA_REPO` and `SPIRA_RELEASE` are `<rel>` (every
service template sets `Environment=SPIRA_RELEASE=` and `Environment=PATH=` from it,
sp-31gtu), `SPIRA_PROD_COCK` is
`<rel>/cockpit`, and each `SPIRA_<X>_BIN` is `<rel>/bin/<its binary>`. Host keys
(`SPIRA_RUN`, `SPIRA_DB`, `SPIRA_INSTANCE`, `SPIRA_COCKPIT`, `SPIRA_DOLT_DATA`,
`SPIRA_TESTDB_DATA`, `SPIRA_TESTDB_PORT`, `SPIRA_SNAP_STALE_S`, `DOLT`) come from the
environment, else `spira.toml`, and `DOLT` from `PATH`. **A key a template uses with no
value is a refusal** naming the key and the template; render.py silently wrote an empty
string. **`SPIRA_PATH_TAIL`** is the one exception to that refusal (sp-c7b85): the box's own
tool-directory tail (`spira.path`, or its `SPIRA_PATH` environment override), appended after
`Environment=PATH=`'s system directories — empty when nothing is configured, which is the
ordinary case, not a refusal. `host_values` itself refuses (naming the entry) before
rendering anything when a tail segment resolves inside a release or a checkout.

**Live aeons.** `activate.sh` refused while aeons ran. Activation here never restarts an
aeon (they are transient units with no file in the unit directory) and never restarts a
oneshot mid-run, and an aeon's tools stay on disk in its release. So there is no guard.

### Hotfix: activate an unlanded commit

`activate <sha> --hotfix "<reason>"` activates as above and records
`$SPIRA_RUN/release/hotfix` (`sha`, `reason`, `at`). `release status` prints
`RUNNING UNLANDED <sha>: <reason> (since <at>)` while it stands, and, once it has stood at
least `hotfix_alert_hours` (env `SPIRA_HOTFIX_ALERT_HOURS`, else `spira.hotfix_alert_hours`,
else 4), an additional `ALERT hotfix <sha> standing <n>h >= threshold <n>h` line
(sp-6p20x). Every consumer reads these two lines from `release status`'s own text rather
than re-deriving the age or the threshold: `doctor.sh`'s `doctor_check_hotfix` (WARN on
`RUNNING UNLANDED` alone, FAIL once `ALERT` joins it), the cockpit ops pane
(`spira/cockpit.sh`'s snapshot carries them as `SP_HOTFIX_LINE` / `SP_HOTFIX_ALERT`;
`cockpit/health.sh`'s `hotfix_banner` renders them), and `watchtower.sh` (files a bead once
per standing hotfix — `SPIRA_INCIDENT_REF=incident:hotfix-<sha>`, so incident.sh's own
dedup bumps a recurrence rather than piling up beads while the same one stands).

A later activation **without** `--hotfix`, while a hotfix stands, **supersedes it only if
the hotfix commit is an ancestor of both the new commit and `--landed-ref`** (default
`local/main`) in `--repo`. Otherwise it is refused, naming the hotfix, until the fix lands or
is rolled back. A second `--hotfix` replaces the first. `rollback` off a hotfix clears the
record; `rollback` onto a release that was activated as a hotfix restores it.

### rollback

`$SPIRA_RUN/release/history` is a stack, one activation per line
(`<sha> landed|hotfix [reason]`). `activate` pushes; `rollback` requires the top to be what
`current` names (otherwise the record and the system disagree, and it refuses rather than
guess), activates the entry below it, and pops. Beyond `current`, this and the hotfix record
are the only mutable state, and both live in `$SPIRA_RUN`.

### prune

Keep the newest `--keep` releases (else `$SPIRA_RELEASES_KEEP`, else
`spira.releases_keep`, else 5; about 65 MB each) by the MANIFEST's `built` time. Never
removed, whatever their age: what `current` names, the rollback target, a standing hotfix.
Stage directories whose builder process is gone are removed too. A release is made writable
before removal; one that still cannot be removed is reported, and prune exits 1.

### install-tarball

The public pipeline's own install (`spira/activate.sh`'s replacement): unpack a tarball
`spira/build-tarball.sh` built beside existing releases, swap `current` atomically, and
restart. It is a second, timestamp-named release lineage alongside `build`'s sha-named one
(DESIGN.md "install-tarball: differences from activate" below), and the two never share a
directory.

1. `<tarball>` must be `spira-<timestamp>.tar.gz` (or `.tgz`); the release name is the
   basename with the extension stripped — the tarball and the directory it unpacks to name
   each other, exactly as `build-tarball.sh`'s own NAMING section documents. A name that
   does not fit this shape is a refusal before anything is touched.
2. If `spira-releases/<name>` already exists it is not re-unpacked (releases are immutable,
   and this is the mechanism `deploy.sh`'s rollback depends on: re-running `install-tarball`
   against a tarball already installed is a cheap re-activation, not a rebuild).
3. Otherwise: extract into a staging directory beside the release dirs, confirm it produced
   the promised `<name>/` (a mismatch is a refusal, nothing is moved), **move it into place
   before marking it read-only** — renaming a directory into a different parent rewrites its
   own `..` entry, which needs write permission on the directory being moved, so a read-only
   source would make every install fail at the rename (`spira/activate.sh` chmods after its
   own `mv` for the same reason; sp-jsnbm found this the hard way, in this crate's own unit
   tests, before it ever reached a real install).
4. Atomically swap `current` (a temp symlink beside it, renamed over it — same mechanism as
   `activate`).
5. `daemon-reload`, then restart every **active** `spira-*.service` unit that is not
   transient. Best-effort: a `daemon-reload` or restart failure is a warning, not a refusal
   — `deploy.sh` already wraps the whole call in its own coarser-grained rollback.
6. Prune: keep the newest `--keep` (`cfg.keep`, the same key `build`/`prune` read)
   timestamp-named releases, newest name first (the name is the timestamp, so lexical order
   is chronological order), excluding whatever `current` now names. Best-effort, reported,
   never fails the install.

`--dry-run` reports the same three intentions `activate.sh --dry-run` printed and changes
nothing.

#### install-tarball: differences from `activate`

Kept deliberately close to `spira/activate.sh`'s own behaviour rather than upgraded to
`activate`'s stricter one:

- **Every active `spira-*.service` unit restarts, not only the ones whose `ExecStart`
  changed.** A tarball install has no prior release's rendered units to diff against — that
  diff is what `activate`'s unit-render step (DESIGN.md "activate") produces, and
  `install-tarball` never renders units at all (`install.sh --skip-build`, run separately by
  the caller, does that for the public pipeline).
- **A `daemon-reload` or restart failure is a warning, not a rollback.** `activate` rolls
  back the whole switch when a restarted unit fails to come up; `install-tarball` does not,
  because `deploy.sh` (its only caller) already wraps the call in its own rollback at the
  whole-deploy grain, and a public-pipeline install has no "previous unit files" to restore
  to in the first place (it never renders any).
- **No live-aeon guard.** `spira/activate.sh` refused while aeons were live
  (`SPIRA_ACTIVATE_FORCE=1` overrode it) — installing while aeons held worktrees under the
  release directory destroyed in-flight work (scar: sp-s6hk). Under the "running system is a
  release" design (DESIGN.md's own Intent) an aeon's worktree is never under
  `spira-releases/`, so there is nothing left to disrupt, and the same reasoning
  `activate`'s own "Live aeons" section already gives applies here unchanged; the guard and
  its override are dropped rather than ported.
- **A bad tarball name or missing file is a refusal (exit 1), not a usage error (exit 2).**
  It is a domain refusal, like "no release `<sha>` in ..." from `verify::release_dir` — the
  crate's own convention (DESIGN.md "Contract": "`2` usage") reserves 2 for CLI
  argument-parsing errors, not for a malformed argument's content.
- **The prune keep default is `cfg.keep` (5), not `spira/activate.sh`'s own hard-coded 100.**
  Both still respect `SPIRA_RELEASES_KEEP` identically; only the unconfigured default moves,
  to the one the rest of this crate already uses (DESIGN.md "prune").
- **No cryptographic MANIFEST check.** `build-tarball.sh`'s own MANIFEST only hashes the
  shipped binaries, not every tracked file (`build`'s MANIFEST hashes everything) — the two
  are different, incompatible shapes, and `spira/activate.sh` never checked either. Verifying
  the tarball's own promise (a real commit exists, the binaries it lists are present) is
  `build-tarball.sh verify`'s job, run separately in the release workflow, not
  `install-tarball`'s.

### stage

`spira/stage.sh`'s replacement: stand up (or tear down) a fully isolated Spira for
testing — its own git repo and bare remote, its own beads database (`bd-embedded`, via a
private `bin/bd` symlink so nothing this stage runs can resolve the caller's real `bd`), its
own `SPIRA_RUN`, a minimal chamber (one `canary` fayth, `FAYTH_MAX_CONCURRENT=1`), a
six-column `repo-map` line, and the two synthetic dispatch scripts `sentinel` needs in place
of `systemd-run`: `fake-summon.sh` (execs `release canary-worker` synchronously) and
`fake-launch.sh` (records the landing dispatch timestamp and exits 0 — `release canary` runs
`landing-pass land` directly so it controls the timing).

Every non-test `.sh`/`.py` file directly under `$SPIRA_RELEASE/spira/` (the release stage is
run from, never a checkout — DESIGN.md's own Intent) is symlinked into the stage's own
`spira/`, so a stage runs the actual code, not a copy frozen at setup. Isolation is asserted
at build time: `up` refuses to return an env block where `SPIRA_DB`, `SPIRA_RUN` or
`SPIRA_HOME` resolves outside the stage root, the same assertion `stage.sh` made. `down`
refuses a path with no `spira/chamber/canary.fayth` — the one check that stands between it
and an arbitrary `rm -rf`.

`SPIRA_SCOPE_LABEL` is always empty in a stage's own env, never read from the caller's: the
`spira.conf` default of `spira` would make sentinel query for `spira,plan` while the bead is
created with `plan` only, and `plan_ready` stays 0 (the same reasoning `stage.sh` documented).

### canary

`spira/canary.sh`'s replacement: an end-to-end pipeline canary on a `stage`. Files a
synthetic bead, runs the real `sentinel` (which dispatches `release canary-worker` via
`SPIRA_SUMMON`), runs the real `landing-pass land` directly, and asserts a commit naming the
bead id appears on `origin/main` inside `--deadline` (default 120s). **The one thing that is
fake is the model:** `canary-worker` claims, commits and closes the bead without invoking
Claude — the model is the one part canary cannot test, and `SPIRA_SUMMON`/`SPIRA_LAUNCH` are
exactly that boundary.

On failure it files a `spira,incident` bead in the **production** database — the caller's own
`SPIRA_DB`, captured before a stage's env (own or `--stage`'s) replaces it — never the
stage's, and never lets a forensics failure change canary's own exit status.

`canary-worker` claims the first ready bead in the canary partition (`bd ready --limit 0
--exclude-type epic,event -u --claim --label <scope,>plan --json`), creates branch
`spira/<id>`, commits a marker file as `canary`/`canary@example.invalid`, pushes, records
`branch=<name>` on the bead, and closes it. It is invoked only by `fake-summon.sh`, inheriting
the full stage env; there is no per-stage copy of it (unlike `stage.sh`'s own
`canary-worker.sh`, written fresh into every stage) because it is a subcommand of the one
binary already on every launcher's `PATH`.

### acceptance

**sp-ak7qm** (rewrite wave 3) moves `spira/acceptance-run.sh` and the library it sourced,
`spira/acceptance-lib.sh`, into this crate. Both scripts are deleted.

**Intent.** Before a release is published, prove on a genuinely clean machine that its
tarball installs, lands a bead end to end, and uninstalls. When a predecessor is given, also
prove that it upgrades from the predecessor, rolls back to it, and upgrades a populated,
aged install without losing state. The verdict is one line, and a git note on the tag records
it. A PASS has to mean the thing works. So every check reports what it looked at, and a check
that cannot look reports FAIL, never a pass.

It lives in `release` because it drives `install-tarball` and nothing else in the workspace
knows the release lineage. It also gets onto a clean machine for free. The acceptance workflow
takes the candidate tarball's own `bin/release` and runs `release acceptance`, so the
acceptance logic comes from the same commit as the artifact under test. Before, it came from
the tag's checkout, which was the same commit by another route.

```
release acceptance <tag> --scratch-repo <path> [--prev-tag <tag>] [--record]
                   [--notes-repo <path>] [--file-defects] [--bd-db <path>]
                   [--agent <path>] [--waive-upgrade] [--tarball <path>]
                   [--prev-tarball <path>]
```

The flags keep the script's meanings (`--tarball`: phase A uses a local file and every deploy of
`<tag>` gets `deploy.sh --tarball`; `--prev-tarball` does the same for `<prev-tag>`;
`--waive-upgrade` clears any `--prev-tag` and skips B/C/D, and the note says so). The one new
flag is `--notes-repo`. Environment knobs are unchanged: `SPIRA_ACCEPT_SUMMON_SECS` (180),
`SPIRA_ACCEPT_ASSET_WAIT_SECS` (600), `SPIRA_ACCEPTANCE_FORENSICS`, `SPIRA_NOTES_REPO`,
`SPIRA_FORGE_REPO`/`GH_REPO`/`GITHUB_REPOSITORY`.

Exit `0` every check passed; `1` a check failed; `2` usage, or a failed prerequisite.

**Phases.** The same four, with the same check names, so a log from the script and a log from
the binary compare line for line. The prerequisites are the positive control of the tool lookup;
`bd`, `dolt`, `git`, `gh` and `python3` on `PATH` (`install.sh` and `systemd/render.py`
still need python3); a working `systemctl --user`; and a git scratch repo. **A** checks the
positive control of the binary check, then acquires the tarball (a bounded wait for the forge
asset, sp-5olmi), stages it into the local release source, and runs `install-tarball`. It then
requires every native binary, runs `install.sh --skip-build`, and requires `ready.sh` to
exit 0. It files a probe bead, requires the builder predicate to be able to claim it, and
follows it through summoned, committed, closed-or-submitted and landed by ancestry on
`origin/<land ref>`. Finally it uninstalls and requires no `spira-*` unit to remain. **B**
installs `<prev-tag>`, snapshots the unit set, runs `deploy.sh --allow-draft <tag>`, and
starts every oneshot. It requires the `.tags/<current>` sidecar to name `<tag>` and
`SPIRA_PROD` to point into the releases directory. **C** runs `deploy.sh <prev-tag>`, starts
the oneshots, and requires the unit set to equal B's snapshot. **D** installs `<prev-tag>`
over the surviving state, seeds beads and statutes, writes an operator override
(`SPIRA_CHECK5_MAX_FILE`, a key conf.sh honours), starts the world, and deploys `<tag>`. It
then requires bead and memory counts to be preserved, `doctor.sh` to exit 0, the override to
survive, no unit to fail within 2 min, and the world to be live, and requires a second probe
bead to land. The forced rollback must either succeed with a live world or refuse and name
the migration.

**Schema.** Each check prints `  ok    <name>` or `  FAIL  <name>: <reason>` on stdout and
writes one JSON line to `<forensics>/checks.jsonl`:
`{"phase","check","verdict":"ok"|"fail","reason"?,"ts","elapsed"}`. The first failure in a
phase takes a forensics snapshot, `<forensics>/NN-first-fail-<phase>/`, and the end of the
run takes one too. A snapshot holds the timers, unit status, per-unit journals, `bd`
list/ready/probe, the run dir, the instance config directory, the scratch repo's refs, `ps`,
`free` and `df`. A forensics failure never changes the verdict. The summary is
`<pass> passed, <fail> failed`, followed by `verdict: PASS|FAIL  tag=<tag>  date=<rfc3339>`.
The note on `refs/tags/<tag>` under `refs/notes/acceptance` has this form:

```
PASS|FAIL
<rfc3339> <tag>  <n> passed, <m> failed
sha256:<hex> tarball:<name>            (when a tarball was acquired)
aged-install from=<prev>: PASS|FAIL    (when a predecessor was tested)
upgrade phases waived by operator      (when waived)
```

**Structure, where the script had text.** The `acceptance-run` spira-lint rule held the
script to about 55 textual invariants, because the script ran for real only on a clean
machine. In Rust each one either is structural or is a unit test:

- There is one `tool()` constructor for `deploy.sh`, `uninstall.sh`, `world.sh`, `doctor.sh`
  and every `conf.sh` read. So "every call runs under `_ci_deploy_env`" has no other way to
  be written.
- There is one `deploy_tag()` that always passes `--allow-draft`.
- There is one `start_sentinel()` that resolves the instance-suffixed unit from
  `list-unit-files`.
- There is one probe-bead routine shared by A and D, labelled `acceptance,<plan>,<scope>,repo:<scratch>`.
- The override key is a constant, and a test checks it against `conf.sh`'s `SPIRA_CONF_KEYS`.

Tests run the whole run, all four phases, against a fake `Host` and a fake clock. They cover
a PASS, each class of FAIL, and the waiver.

**Decisions** (behaviour dropped or changed, deliberately):

1. **The `release` that installs a tarball is this binary itself** (`current_exe`), not
   `${SPIRA_RELEASE_BIN:-release}` from `PATH`. On a clean machine nothing had put a
   `release` on `PATH`, so the bare name could not resolve there.
2. **Everything the release runs gets the release's launcher environment**:
   `SPIRA_RELEASE=<releases>/current` and `PATH=current/bin:current/spira:$PATH`. That covers
   `install.sh`, `ready.sh`, `deploy.sh`, `uninstall.sh`, `world.sh`, `doctor.sh` and the
   `conf.sh` reads. The script put only `current/bin` on PATH, and only for the last four. It
   then called `deploy.sh` and the others by bare name (sp-gypjk), but they live in
   `current/spira/`, which that PATH never reached. `install.sh` ran on the caller's own PATH
   and so could not resolve `doctor.sh` or `spira-config`. Both are phase A FAILs on
   local/main: `release: command not found`, then `uninstall.sh` exit 127.
3. **`conf.sh` is read from the release under test** (`current/spira/conf.sh`), not from
   the checkout the script sat in. There is no checkout: a release is the only thing that
   runs.
4. **`--notes-repo` replaces the script's own checkout** (`$HERE/..`) as the repository the
   note is written to and the forge repo is derived from. It defaults to
   `$SPIRA_NOTES_REPO`. **`--record` with neither is a usage error (exit 2)**, where the
   script always had a checkout to fall back on.
5. **A count that cannot be read is a FAIL.** `bd list --all --json | python3 … || printf 0`
   read an unreadable store as 0 both before and after the upgrade, and 0 ≥ 0 passed. Now
   phase D fails when it cannot count. The same applies to a `SPIRA_RELEASES` that the
   release conf does not yield: phase B's sidecar check no longer guesses
   `$HOME/spira-releases` in its place.
6. **The per-step `$TMP/*.log` tee copies are dropped.** `$TMP` was removed at exit, so no one
   could ever read them. Tool output streams to stdout, as it did through `tee`.
7. **The default forensics directory outlives the run.** It is a `mktemp`-style directory of
   its own, not `$TMP/forensics`, which the script deleted at exit while printing its path.
8. **`journalctl --since`** carries an explicit `UTC`. The script passed a UTC wall time
   that journalctl read as local time.
9. **JSON is parsed with serde_json.** The script parsed it with inline python. A reply that
   cannot be parsed still counts as "not claimable" or "not finished", as before. Every reader
   parses **stdout alone**, which is the script's `2>/dev/null`. The first cut appended stderr
   to stdout, and in a real run a bd warning after the JSON turned a claimable probe into a
   FAIL.
10. **Unit lists are read with `--plain`.** Without it, `systemctl list-units --state=failed`
    prefixes a failed unit with `●`. The script's `awk '{print $1}' | grep '^spira-'` then
    dropped exactly the units it was looking for, so the phase D crash-loop check passed
    vacuously.

### session-hook

**sp-7jr34** (rewrite wave 6b) moves `spira/install-session-hook.sh` here. The script is
deleted.

**Intent.** The coding agent client's `SessionStart` hook and status line are registered in
its own settings file, outside every checkout — no landing gate can see that file and nothing
that lands can fix it. Exactly one Spira entry for each must ever be registered, addressed so
that it keeps working across every future release activation without being re-registered.

**THE DEFECT THIS BEAD FIXES (P0, found in the bead's own comments).** The script computed its
registered command from `SPIRA_HOME`, which a release activation sets to that release's own
sha-pinned directory (`spira-releases/<sha>/spira`, never `current` — DESIGN.md "activate":
"never current, so a unit states exactly what it runs"). `install`'s own idempotence only
stripped a command byte-identical to the CURRENT invocation's own path, so every activation
registered a command the next activation's `install` did not recognise as "ours" — 27 had
accumulated before this was found, and the same shape was found again, once, in the course of
fixing it (a leftover bare sha-pinned entry the hand-fix had missed) — proof the fix must
recognise every earlier form, not just exact matches. Every one of the 27 exited 1: a hook
registered as a bare path runs under the CLIENT's own environment, which does not carry the
launcher's PATH, so `conf.sh` could not find `spira-config` and failed closed. The status line
had no installer at all and was wired by hand into the identical broken shape.

**The fix.** The registered command is always addressed through `<releases>/current` — never a
sha directory — and is SELF-CONTAINED: `env SPIRA_RELEASE=<releases>/current
PATH=<release_path_with_tail> <releases>/current/spira/hooks/session.sh` (and the same for
`ctx-meter.sh`, as `statusLine`). It carries its own environment regardless of what invoked it,
so it no longer matters which release's `install-session-hook.sh` — now `release
session-hook` — happened to run it. "Ours" is recognised by a command SUFFIX
(`spira/hooks/session.sh` / `spira/ctx-meter.sh`), not by exact match, so `install` converges
any number of earlier forms — sha-pinned, bare "current", wrapped or not — to exactly one
entry, on both `hooks.SessionStart` and `statusLine` (a test plants three sha-pinned entries
plus a bare "current" one and asserts `install` leaves exactly one:
`session_hook::tests::three_activations_converge_to_one`).

**Why `install`/`status` need a release and `uninstall`/`prune` do not.** Only `install` and
`status` need to construct the canonical command, which needs the releases directory
(`Config`). `uninstall` and `prune` touch only the settings file and recognise entries by
suffix alone — resolving `Config` for them would refuse an uninstall on a box whose releases
directory cannot be found, which is exactly the state an uninstall may be reached from. `main.rs`
resolves `Config` only inside the two branches that need it.

**The `current`-or-`SPIRA_RELEASE` fallback** (`release_root_for_session_hook` in `main.rs`).
`install`/`status` prefer `<releases>/current` when `Config` resolves AND that path actually
exists — production's own shape, where a release rotates and `current` never does. When
either is untrue (no `spira.toml`/`SPIRA_RELEASES` at all, or a releases directory with no
`current` link yet), they fall back to `$SPIRA_RELEASE` itself as the release root directly,
with no `current` join. This never fires in production; it fires in an environment with
exactly one release and no rotation at all — `concierge.sh start`'s own test fixtures, and
`testenv`'s synthetic per-run release — where "through `current`" has no meaning to begin
with, and refusing to register anything would be strictly worse than addressing the one
release that exists.

**What is kept vs. dropped, against the script:**

* Same four verbs, same `SessionStart`-only, no-matcher registration, same `PostCompact`
  retirement (a stale registration from before `SessionStart`'s own `source=compact` was known
  to cover it), same `prune <substring>` as the deliberate, one-at-a-time way a foreign entry
  is removed.
* **`statusLine` is now managed too** (bead requirement 1) — the script never touched it; it
  had been wired by hand into the exact shape that broke. `install` refuses (changing nothing)
  when either the hook or the meter file is not an executable file — a registered command that
  does not exist is a hook error at every session start, worse than one still absent.
* **The settings document round-trips through `serde_json` with `preserve_order`** (the
  crate's `Cargo.toml` opts the whole workspace build into the feature — additive, changes no
  other crate's correctness, only the order `Value` iterates and serialises in), so a rewrite
  of the operator's live settings file does not reshuffle every other key into alphabetical
  order. The python script preserved order by construction (a plain `dict`); `BTreeMap`-backed
  `serde_json::Map` would not have.
* **Backup and atomic write are unchanged**: `<path>.spira.bak` written only when the prior
  content differs from what is about to be written, `write_atomic` (temp file, `rename(2)`).

**Requirement 3's own test.** `release/tests/session_hook_minimal_env.rs` builds a real release
fixture (a freshly-built `spira-config` binary, the checkout's own real `spira/`), runs
`session.sh` and `ctx-meter.sh` through the REGISTERED COMMAND `session_hook::install` writes,
under `env -i HOME PATH=/usr/bin:/bin` — the client's own minimal environment, never the
launcher's — and asserts `rc=0` and non-empty output. A second test runs the OLD, unwrapped
bare-path form under the same minimal env and asserts it FAILS, so the first test's pass is
known to be about the fix and not about a fixture that would have passed either way
(law-absence-needs-a-positive-control).

### intake

**sp-7jr34** (rewrite wave 6b) moves `spira/install-intake.sh` here. The script is deleted.

**Intent.** A systemd alert template's `OnFailure=` reaches a human (Pushover); a drop-in adds
a second `ExecStart=` that also files the event as an incident bead, so it becomes work with an
identity instead of a notification that scrolls past. A drop-in survives the alert template
being re-rendered by its own repo's deploy, which an edit to the unit file itself would not.

**No default glob.** `SPIRA_ALERT_GLOB` names the caller's own alert units (a `find -name`
pattern); unset, every subcommand is a deliberate no-op (prints why, exits `0`) — a default
would be one box's inventory, and the wrong one would silently wire nothing while reporting
success. On this host it is unset today, so `intake` is presently inert in production; the
mechanism is exercised end-to-end by this crate's own tests with a planted template.

**Kept from the script, unchanged in shape:** the drop-in filename and content
(`50-spira-intake.conf`, a second `ExecStart=-<incident.sh> systemd %i`, the leading `-` so a
failure here cannot fail the alert), writing only the templates whose drop-in would actually
change, a `daemon-reload` only when something changed, and the post-write verify against
`systemctl --user cat <template>@probe.service` (a template's own always-nameable instance,
so `cat` can show the merged unit including the drop-in without that instance ever running).
`find` is still shelled to (behind a [`intake::Templates`] trait, so tests never run a real
one) rather than reimplemented, because `find -name`'s glob (`*`, `?`, `[...]`) is exactly what
`SPIRA_ALERT_GLOB`'s own documentation promises and a hand-rolled matcher is a second place for
that promise to drift from what it actually does.

**The `Systemctl` trait gained one method, `cat`** (`systemctl --user cat <unit>`), for this
verify step alone — `activate`/`rollback`/`install-tarball` never call it.

**Needs no `Config` at all** — only the systemd unit directory
(`config::unit_dir_from_env`, the same fallback chain `Config::resolve` uses for its own
`unit_dir` field, pulled out to a standalone function precisely so `intake` is never coupled to
the releases directory resolving, which it has nothing to do with).

## Layout

| module | what |
|---|---|
| `workspace` | `expected_bins(tree)`: the workspace's `[[bin]]` names from its own manifests |
| `manifest` | write, parse and check `MANIFEST` |
| `config` | resolve the releases dir, run dir, keep, unit dir and host keys |
| `build` | archive, cargo, bins, clash check, MANIFEST, read-only, rename |
| `verify` | the checks above |
| `units` | installed-unit ↔ template mapping and the renderer |
| `systemctl` | the `Systemctl` trait; the real one runs `systemctl --user` (`$SPIRA_SYSTEMCTL`) |
| `git` | the `Git` trait (resolve, archive, is-ancestor) |
| `activate` | activate, rollback, the hotfix rule, history, status |
| `prune` | prune |
| `install` | `install-tarball`: unpack, swap, restart, prune a timestamp-named release |
| `stage` | stand up / tear down an isolated Spira; the fayth, repo-map and dispatch-script content |
| `canary` | the end-to-end canary run, and `canary-worker`, the synthetic aeon |
| `acceptance` | `release acceptance`: the release acceptance run (phases A-D), its checks, forensics and note |
| `session_hook` | `release session-hook`: the client's `SessionStart` hook and `statusLine` |
| `intake` | `release intake`: systemd alert templates wired to `incident.sh` via a drop-in |

Unit tests run activation and rollback against a fake `Systemctl` and a temporary unit
directory; `install-tarball` against a fake `Systemctl` and a fake `Unpack` (no real `tar`);
`stage`'s pure content (the fayth, the repo-map line) and its build-time refusals directly;
`intake` against a fake `Templates` (no real `find`) and the same fake `Systemctl`.
`session_hook`'s JSON transformation is pure and needs no fake at all; one further test
(`release/tests/session_hook_minimal_env.rs`) runs the real `session.sh`/`ctx-meter.sh`
through a really-built `spira-config` (sp-7jr34's own requirement 3). Nothing in a test
touches the host's systemd, a real tarball, or a real `bd`/`git`/`sentinel` — `stage` and
`canary`'s own orchestration is proved instead by running them for real, once, against a
scratch `STAGE_ROOT` and the real `sentinel`/`landing-pass` binaries (this bead's own delivery
evidence; `test-canary.sh` repeats this under the gate/round).

## Callers

`queue land-local` builds (`--bin-dir`), verifies and activates every landing of the harness
while a release is in force, and `queue rollback-local` verifies and re-activates the previous
round's (sp-gkfg1, queue/DESIGN.md §8 D13); `skew.sh refresh` in release mode builds,
verifies and activates the checkout's head. The Makefile's `install` target is deleted.

`spira/build-tarball.sh` remains for the public pipeline only (sp-jsnbm keeps it bash: it
produces the tarball this crate's `install-tarball` consumes, a separate concern from
unpacking one): the GitHub release workflow builds its tarball with it
(`.github/workflows/release.yml`, `make dist`, `acceptance-local.sh`). `install-tarball`
replaces `spira/activate.sh`, which is deleted: `deploy.sh` (`SPIRA_ACTIVATE_SH`, default
`release`, now takes `install-tarball` as its own first argument),
`release acceptance` (which runs its own binary's `install-tarball` with `SPIRA_RUN` as a
sibling of the releases dir — DESIGN.md "install-tarball" resolves `state_dir()`
unconditionally; sp-ak7qm replaced acceptance-lib.sh's `_install_from_tarball`) and
`install.sh`'s own messages. `release acceptance` replaces `spira/acceptance-run.sh` and
`spira/acceptance-lib.sh`, which are deleted: `acceptance-ci.sh` (acceptance.yml) and
`acceptance-local.sh` run the candidate tarball's own `bin/release acceptance`. `stage`/`canary` replace `spira/stage.sh` and `spira/canary.sh`,
which are deleted; today's only caller is their own suite, `test-canary.sh`. `systemd/render.py`
remains for `install.sh` and `unit-ensure.sh`, which own unit membership.

`release session-hook` replaces `spira/install-session-hook.sh`, which is deleted: every
former bare-name caller is repointed — `concierge.sh`'s `start` (no longer overrides
`SPIRA_HOME` first: the registered command is addressed through `current` regardless of which
release's `concierge.sh` runs it, so there is nothing left to override), `install.sh`'s and
`systemd/install.sh`'s own phase 5, `spira/owned.sh`'s `_session_hook_status`, and
`spira/uninstall.sh`'s removal step. `cockpit-ensure.service`'s `ExecStartPre` is repointed to
`@SPIRA_PROD_ROOT@/bin/release session-hook install` (was `@SPIRA_PROD@/install-session-hook.sh
install` — `@SPIRA_PROD@` is the sha-pinned `spira/install-session-hook.sh` is deliberately no
longer addressed by, DESIGN.md "session-hook"). `release intake` replaces
`spira/install-intake.sh`, which is deleted: `install.sh`'s own phase 5 and
`spira-ops.service`'s `ExecStartPre`, repointed the same way to `@SPIRA_PROD_ROOT@/bin/release
intake install`. `spira/uninstall.sh`'s ALERT DROP-INS step never called
`install-intake.sh` (it removes each drop-in file directly, by the paths `owned.sh` names) and
needed no change beyond its own comments.

## Not this bead

Launchers that set PATH (sp-31gtu, sp-gypjk).

## Build IO (sp-z61hj)

`release build` without `--bin-dir` compiles through the box's sccache
(`spira-config/DESIGN-build-cache.md`): absent sccache is an error, `SPIRA_BUILD_CACHE=off`
opts out loudly. The target is passed as `--target-dir`, never `CARGO_TARGET_DIR` (sccache
hashes every `CARGO_*` variable, so an env target would give every release its own cache
keys), and a caller's `CARGO_INCREMENTAL` is removed.

