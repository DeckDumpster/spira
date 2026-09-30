# install — design

Rewrite wave 6a (sp-31dm0, brain `wiki/projects/spira/remaining-bash-inventory.md` group 6).
Replaces `install.sh` (the root installer), `systemd/install.sh` (the per-instance unit
renderer/writer), `systemd/unit-ensure.sh` (its non-disruptive sibling), `systemd/units.sh`
(the unit-naming library) and `systemd/render.py` (the template substitution engine).
`spira/install-session-hook.sh` and `spira/install-intake.sh` were retired by sp-7jr34,
concurrently with this bead, into `release session-hook`/`release intake`; this crate's
call sites were updated to match at merge time (merged from local/main before landing) —
not re-ported here, since they were never this bead's scope.

## Intent

A box goes from a bare clone (or release tarball) to a running Spira instance through nine
phases: preflight, conflict checks, config, build, database, units, hooks, cockpit, verify.
Two of those phases — units, and its non-disruptive sibling `unit-ensure` — are load-bearing
enough to be called directly by other tooling (`deploy.sh`'s re-render step, `landing-pass`
after every pass), so they are also standalone binaries, not just a phase of the root
installer.

**Retire rather than port** (law-rust-rewrites-start-from-intent): every function in the five
scripts was read for a live caller (`git grep`, including systemd unit templates, chamber
briefs, workflows, other scripts and Rust crates) before being ported. Nothing was found with
no caller — the five scripts are old enough, and small enough (2,515 lines total), that
nothing had accreted an orphaned code path. What did accrete, and what this rewrite drops, is
listed under **Decisions**.

## Contract

Three binaries, one crate:

```
install [<instance>] [--dry-run] [--ephemeral] [--laptop] [--skip-build]
        [--no-session-hook] [--system-user]
    The root installer. Exit: 0 ready, 1 preflight refused, 2 a phase failed,
    3 installed but not ready, 5 conflict (phase 0.5).

units-install [<instance>] [--diff | --render | --laptop] [--no-migrate-watchers]
    Render every template this box installs, write what changed, migrate legacy unit
    names and cockpit state, enable/restart/prune per unit, and report the end state.
    `deploy.sh`'s re-render step and `install`'s own phase 4 both call this logic (the
    binary directly, or in-process — DESIGN.md "Callers").

unit-ensure [--diff]
    The non-disruptive sibling: installs missing/changed unit files and daemon-reloads,
    but never restarts a running service. `landing-pass` runs this after every pass.
```

## Schema (modules)

| module | replaces | owns |
|---|---|---|
| `values` | `systemd/render.py` | Host value map (systemd/render.py's `--home`/.../`--path-tail` flags) and the render call itself — delegates to `release::units::render_from_values`, the shared core [[render-refactor]] extracted from `release::units::render` so `release activate`/`install-tarball` and this crate's renderer are the same code, not two that can drift. |
| `manifest` | `systemd/units.sh` | Which templates this box installs, which get enabled, `inst_name`/`inst_watch_name`, the watcher list. Pure data over an `Inputs` struct the caller resolves (dolt/testdb presence, `inotifywait`, the watcher manifest) — no filesystem, no `watchd.sh`, inside the module itself. |
| `decide` | `systemd/install.sh`'s `_unit_action` | The six-input per-unit apply decision, already extracted to a sourceable bash function before this rewrite (docs/test-plan/instance-lifecycle.md UC-22); ported as the pure function it already was. |
| `guards` | `systemd/install.sh`'s `_check_path_collisions`, root `install.sh`'s five conflict predicates, `_prod_guard`, `_configure_prod_guard`, `_bootstrap_decision`, `_db_git_guard`, `_seed_when` | Every pre-flight refusal, as a pure function over already-resolved state (a `/proc` scan, a TCP probe, a parsed sibling `*.conf`) — never the real filesystem or network itself, so a test drives every branch without root, a container, or systemd. |
| `systemctl` | (new: `systemd/install.sh`'s and `unit-ensure.sh`'s inline `systemctl --user ...` calls) | A `Systemctl` trait (enable/enable-now/disable-now/restart/kill/is-active/is-enabled/list-matching) with a real (subprocess) and fake (in-memory) implementation. Richer than `release::systemctl::Systemctl` (which only needs reload/state/restart/list-active for `activate`), so it is its own trait rather than widening that one under every implementor. |
| `install_units` | `systemd/install.sh`'s main body | Render, write, mask detection, ExecStart executability, `--render`/`--diff`, the enable/restart/prune loop (via `decide`), legacy-unit migration, cockpit-state migration, prune-target computation. |
| `ensure` | `systemd/unit-ensure.sh` | The same render/write, without restart: enable+start only newly-installed units, the MISSING-TARGET guard, and the broker-producer guard run on every invocation. |
| `checks` | root `install.sh`'s path-collision/landref/live-aeon refusals as `units-install` runs them | Shared between `units-install` and `install`'s phase 4 so the two cannot resolve a different answer from the same box. |
| `orchestrate` | root `install.sh`'s phase sequencing (the parts worth unit-testing without doctor.sh/a real dolt server/systemd) | The `Deps` trait for external tools not yet ported, the dolt-query-ready retry loop, phase 4's wiring, `spira-config path-tail`'s logic read in-process instead of shelled out to. |
| `bootstrap` | glue `systemd/install.sh`/`unit-ensure.sh` both had (source conf.sh/units.sh, call `watchd.sh units`, read `ctrl.sh`'s suspended set) | Resolve host values, the manifest and the unit directory from the environment a caller already set — the one place this is done, so `install`, `units-install` and `unit-ensure` cannot each derive it slightly differently. |

The root `install` binary's phase bodies (database bootstrap, spira-lc system-user creation,
cockpit symlinks) live directly in `src/bin/install.rs` — they are almost entirely "shell to a
tool that has not moved to Rust yet, in the right order, under `--dry-run`", which is exactly
what the bash original was and is not more unit-testable by being moved into the library
untested. Their sequencing was verified by reading every phase transition against the bash
line by line, not by a unit test per phase.

## Parity

`install/examples/parity.rs` (scratch, removed before landing) rendered every live template
in `systemd/` — all 63 `.service`/`.timer`/`.yaml` files, including the watcher template with
a plain watcher name — through `install::values::render`, against the same inputs, and
diffed byte-for-byte against `systemd/render.py`'s output for the same inputs on the same
templates. **`diff -rq` reported zero differences.** This is the production config: every
template this box ever actually renders, not a synthetic fixture.

## Decisions

- **The renderer's empty-value refusal is adopted, not render.py's looser rule** (FAIL CLOSED,
  wave brief 2026-09-29). `render.py` refused only an entirely-unfilled placeholder, plus one
  hand-written special case for `@DOLT@`. `release::units::render_from_values` (already shipped
  for `release activate`/`install-tarball`) refuses **any** placeholder used with an empty
  value, `SPIRA_PATH_TAIL` excepted. This crate reuses that same function rather than porting
  the looser rule, so the whole render surface — `activate`, `install-tarball`, `install`,
  `units-install`, `unit-ensure` — shares one behaviour. Checked against every template
  (**Parity**, above): no template renders a key empty on the path that reaches it today, so
  this changes no real verdict, only turns a hypothetical silent-empty render into a named
  refusal. The one wording difference this drops: `@DOLT@`'s old special message ("dolt is not
  on PATH; install dolt before rendering units that need it") becomes the generic "uses DOLT
  but no value is set for it" — the refusal, not the prose, is what mattered.
- **The `$tmpl` "watcher template changed" check in `systemd/install.sh`'s enable loop is
  dropped, not ported.** `tmpl="${u%%@*}@.service"` was meant to mark every watcher instance
  changed when the shared `spira-watch@.service` template changed — but under per-instance
  naming no installed unit name contains `@` any more, so `${u%%@*}` never matches and `tmpl`
  is always a key `_CHANGED` never holds. It is dead code left over from the per-instance
  naming migration, not a check this port owes fidelity to (see the comment in
  `install_units::tests::a_changed_and_active_unit_restarts_rather_than_enables_fresh`, which
  documents the analysis). A changed `spira-watch@.service` template still causes every
  watcher instance to differ on its own: each instance's own rendered content includes the
  template body, so `render_all`/`run`'s per-unit comparison catches it directly — the dead
  check bought nothing even when it could have fired.
- **`loginctl enable-linger` and `release session-hook install` are still called from two
  places** (`units-install`'s own run, and `install`'s phase 4 stamp-file logic / phase 5).
  This is the original scripts' own accreted double-up (root `install.sh` called `systemd/
  install.sh` as a phase, and both independently called these two); both calls are harmless
  and idempotent, so this port keeps them rather than "fixing" a redundancy mid-rewrite that
  was never reported as a defect.
- **`install`'s phase 4 calls `units-install`'s logic in-process, not as a subprocess.** Root
  `install.sh` shelled to `bash systemd/install.sh` because they were two files; one binary now
  makes that a function call. The standalone `units-install` binary still exists for
  `deploy.sh`'s re-render step, which needs it as a separate process pinned to a different
  release's environment (`release/DESIGN.md` "Render").
- **The landref and live-aeon checks are re-derived directly, not shelled into `lib.sh`.**
  `spira_live_aeons` (lib.sh) is a one-line `systemctl --user list-units` query; reimplementing
  it against this crate's own `Systemctl` trait is simpler and more testable than sourcing
  bash for one query, and does not touch `lib.sh` (per Ryan: "leave lib.sh alone" stands —
  nothing here edits it, this only stops depending on it for a query it does not need to be
  bash to answer). The landref git-resolution sequence (repo-map lookup, remote-HEAD fallback,
  `remote set-head`) is *not* re-derived — `checks::check_landref` shells to real `git`
  directly for the same sequence `_install_landref` used, because `spira_landref`'s own
  repo-map/remote fallback logic belongs to `spira-config` the day it gets a typed home, and
  duplicating it a second time in this crate is exactly the drift `sp-c7b85`'s
  `spira-config path-tail` precedent exists to prevent.
- **`spira-config path-tail`'s logic is called in-process** (`orchestrate::path_tail`), not by
  shelling to the `spira-config` binary — this crate already depends on `spira-config` as a
  library, and the whole point of that crate's public `tail_refusals`/`discover`/`load` is to
  be read by Rust callers directly.

## Callers repointed

- `spira/deploy.sh`: `_INSTALL` default (`$HERE/../systemd/install.sh` → `units-install`,
  called by bare name on the release launcher PATH, matching every other sibling tool
  `deploy.sh` already calls this way) and `_render_release_units` (`bash .../systemd/
  install.sh` → `units-install`, same env pinning). Both the rollback path (line ~529) and the
  forward re-render (line ~588) go through the same default. **This is the render step
  sp-r15cf's `--skip-restart` depends on being the one that restarts each changed unit exactly
  once** — `units-install`'s enable/restart loop (`install_units::run`) restarts a unit iff
  `decide()` says `Restart`, once, matching `systemd/install.sh`'s own one-restart-per-changed-
  unit contract exactly (parity, above, plus `install_units::tests::
  a_changed_and_active_unit_restarts_rather_than_enables_fresh`).
- `landing-pass/src/pass.rs`: `self.s.repo.join("systemd/unit-ensure.sh")` → `PathBuf::from
  ("unit-ensure")`, a bare-name binary call through the same `ports::ensure` seam (which
  already branched on "a bare name is a launcher-PATH program, run directly" vs. "a path is
  run with bash" — this just takes the first branch now instead of the second).

## Not this bead

`release session-hook`/`release intake` (sp-7jr34, landed concurrently) are called as
`release` subcommands now, not by the retired script names. `conf.sh`/`lib.sh` (wave 4, last) stay bash; every value this
crate needs from them is read from the environment a caller already resolved, the same
contract `release`'s own `Config::resolve` already established. `doctor.sh`, `configure.sh`,
`build.sh`, `seed.sh`, `mail.sh`, `exclude.sh`, `ready.sh`, `watchd.sh`, `ctrl.sh` are wave 5
("operator surface") or otherwise out of scope; all are called by bare name exactly as before.
