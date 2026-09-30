# release — design

The release producer (sp-m3ipc, work item 1 of the epic "the running system is a release",
brain `wiki/projects/spira/designs/runtime-is-a-release-2026-09-29.md`). It replaces, for
local use, the Makefile's `install` target, `spira/activate.sh` and `spira/build-tarball.sh`.

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
release build <commit> [--repo R] [--target-dir T]      build spira-releases/<sha>, print <sha>
release verify <sha> [--no-pre-activate]                MANIFEST, unit binaries, pre-activate
release activate <sha> [--hotfix "<reason>"] [--repo R] [--landed-ref REF] [--settle S]
release rollback [--settle S]                           activate the previous release
release prune [--keep N]                                keep the newest N releases
release status                                          current, previous, RUNNING UNLANDED
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
   release; else a throw-away `.target-<pid>` beside the stage).
4. Copy every workspace `[[bin]]` target into `bin/`. The list is read from the commit's own
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
is installed.

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
string.

**Live aeons.** `activate.sh` refused while aeons ran. Activation here never restarts an
aeon (they are transient units with no file in the unit directory) and never restarts a
oneshot mid-run, and an aeon's tools stay on disk in its release. So there is no guard.

### Hotfix: activate an unlanded commit

`activate <sha> --hotfix "<reason>"` activates as above and records
`$SPIRA_RUN/release/hotfix` (`sha`, `reason`, `at`). `release status` prints
`RUNNING UNLANDED <sha>: <reason>` while it stands (doctor, the ops pane and watchtower read
that; wiring them is not this bead).

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

Unit tests run activation and rollback against a fake `Systemctl` and a temporary unit
directory; nothing in a test touches the host's systemd.

## Not this bead

Wiring `queue land-local` to build → verify → activate (sp-gkfg1); launchers that set PATH
(sp-31gtu, sp-gypjk); doctor / ops pane / watchtower reading the hotfix record; deleting the
Makefile `install` target, `activate.sh` and `build-tarball.sh` (their callers move first).
