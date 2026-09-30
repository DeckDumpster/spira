# doctor — is the running harness healthy, right now

Replaces `spira/doctor.sh` (634 lines). One binary, `doctor`, the same 14 checks in the same
order. Written from the script's intent and its two mandatory callers (bd sp-yyk47:
`deploy.sh`'s post-activation health check, and `install.sh`'s phase-0 preflight), not
ported line by line — the check-by-check structure is kept deliberately, because
watchtower calling a check directly is a real, load-bearing property (doctor.sh's own
comment: "each its own function so watchtower can call them directly").

## 1. Intent

**Runtime health only.** Not a preflight for a box that has never been installed (that's
`install.sh`'s job, using this as ITS phase 0), not a build check (`pre-activate.sh` /
`release verify` own that). What's left for a box already running: read-only, fast
questions, each independently answerable. Every check is read-only except the events
substrate probe, whose ONE write is a reserved sentinel that matches no real bead.

## 2. What is deliberately NOT touched

- **`conf.sh` stays the one authority on derived configuration.** `doctor` does not
  re-implement conf.sh's key derivation, defaults, or schema handling. `real.rs` sources
  `conf.sh` (under `SPIRA_DOCTOR=1`, exactly as doctor.sh did, so a schema mismatch cannot
  make the check die on the way in) **once at startup** and captures the resulting
  environment; every check reads that snapshot rather than re-sourcing conf.sh per call.
- **`doctor` locates `conf.sh` via an explicit `$SPIRA_HOME` first, release-relatively
  otherwise.** This is the inverse of doctor.sh's own rule (which found conf.sh beside its
  OWN script location, `$HERE`, and never read `$SPIRA_HOME` to find itself at all) — and
  deliberately so. `_activated_release_cmd` (deploy.sh, sp-r15cf) sets `$SPIRA_HOME`
  explicitly, under `env -i` rebuilt from the environment as originally invoked, to the
  release just activated — specifically so a predecessor release's own conf.sh having
  overwritten `$PATH` wholesale (the launcher-PATH invariant, sp-gypjk, predates it) cannot
  matter: the bare-name lookup already resolves the right binary off the pristine `$PATH`,
  and `doctor` must then read the SAME release's `conf.sh`, which is exactly what the
  explicitly-pinned `$SPIRA_HOME` names. `current_exe()`'s release-relative sibling
  (`<release>/bin/doctor` → `<release>/spira/`) is the fallback, for a bare interactive
  invocation with nothing set — the equivalent of doctor.sh's `$HERE` when there is no
  caller-supplied override to honor.
- **lib.sh's specific helper functions** (`spira_deps_list`, `repo_names`, `repo_field`,
  `_bump_write_event_try`, `_counter_events_query`, `spira_unit`, `spira_bin_purpose`) are
  called through the one-shot seam (source conf.sh + lib.sh, run the one function, capture
  stdout) — the same technique `skew`, `gate-check` and `queue` use, for the same reason:
  lib.sh stays the one authority (§skew/DESIGN.md §2 makes the identical argument).
- **`release`, `spira-config`, `overrides.sh`, `watchd.sh`, `gate`, `systemctl`,
  `concierge.sh`** are invoked exactly as doctor.sh invoked them — same argv, same env vars.

## 3. Contract

### 3.1 Invocation and exit code

`doctor` (no subcommands — it always runs every check, exactly as doctor.sh's bottom half
did). Exit 1 if any check is FAIL, else 0. Every line goes to **stdout** (doctor.sh never
wrote to stderr; both mandatory callers capture combined output with `2>&1` regardless, so
this was never load-bearing, but it's kept for exact parity).

### 3.2 Report format, byte-for-byte

```
spira doctor

<section title>
  FAIL  <message>
        <detail line 1>
        <detail line 2>
  warn  <message>
  ok    <message>

...

N fatal, M warnings — the harness is not healthy.   # or: 0 fatal, ... is healthy.
```

Section order and titles are fixed (DESIGN.md §4 lists them); this is what `deploy.sh`'s
`grep -E '^\s*FAIL'` and every retired suite's exact-string assertions depended on, so the
tag column width (`FAIL `/`warn `/`ok   `, both five characters) and the two-space lead
are preserved exactly.

### 3.3 `SPIRA_DOCTOR_INSTALLING` — sp-r15cf, preserved exactly

Two checks soften a would-be FAIL to WARN when `SPIRA_DOCTOR_INSTALLING` is set (non-empty):
**store** (no `.beads` yet; `bd` cannot read because `dolt-beads.service` is not yet active)
and **failed units** (any unit already in the failed state). Both run at `install.sh`'s
phase 0, before phase 0 has touched anything, so what they find there necessarily *predates*
this install and cannot be something the install broke (`doctor_check_failed_units`'s own
comment, carried forward verbatim from sp-r15cf). Outside that flag, both checks stay FAIL.

### 3.4 The two mandatory callers

| caller | invocation | reads |
|---|---|---|
| `install.sh` phase 0 | `SPIRA_DOCTOR_INSTALLING=1 doctor.sh 2>&1` → `doctor` | exit code; prints the combined output indented |
| `deploy.sh` health check | `_activated_release_cmd env SPIRA_DOCTOR=1 "$_DOCTOR" 2>&1` (release-pinned; see §2) | exit code; greps `^\s*FAIL` lines for its own report |

Both are exercised end to end by `test-deploy.sh` and `test-deploy-preflight-new-unit.sh`
(kept, repointed — §6), which is the parity evidence for these two paths specifically, as
the bead's brief requires.

## 4. The 14 checks, in order

`release_tools`, `hotfix`, `config_files`, `chamber_overlays`, `overrides`,
`gate_compile_check`, `store`, `duckdb`, `events_probe`, `failed_units`, `orphan_units`
(same section, "systemd units"), `snapshot_fresh` ("the cockpit"), `operator_channel`,
`concierge_singleton`. Each is a free function `check_<name>(&dyn World) -> Vec<Line>`,
callable on its own — `run()` is only the printer and tallier.

## 5. Schema

```rust
pub enum Level { Ok, Warn, Fail }
pub struct Line { pub level: Level, pub msg: String, pub detail: Option<String> }

trait World {
    fn env(&self, k: &str) -> Option<String>;         // conf.sh's captured snapshot
    fn which(&self, name: &str) -> Option<PathBuf>;    // command -v, or a direct -x test
    fn is_executable_file(&self, p: &str) -> bool;     // [ -x p ], never a PATH search
    ...
}
```

`real.rs`'s `Real::new()` does the one-shot conf.sh capture; every other method is a direct
subprocess call or filesystem read. `tests.rs`'s `Fake` returns canned data per method,
recording nothing beyond what each test needs (unlike `skew`'s `Fake`, most `doctor` checks
don't mutate state, so there is little to record).

## 6. Parity and what moved

**sp-oppza, landed concurrently with this bead and merged in**: `check_config_files` now
runs `spira-config migrate <toml>` immediately before `spira-config validate <toml>`, same
order doctor.sh's own fix used — a box whose config predates sp-k6m1m (a goal set, no
`id_prefix`) is repaired in place instead of failing validation on every such box. Idempotent
(a no-op once `id_prefix` is set, including production's own state); its own exit code is
never read, only its output, which is logged as a plain, unranked `Level::Raw` line (never
folded into the validate verdict) and only when non-empty.

11 dedicated suites are retired, their coverage moved to this crate's 52 unit tests:
`test-doctor-concierge-singleton.sh`, `test-doctor-config-files.sh`, `test-doctor-duckdb.sh`,
`test-doctor-events-probe.sh`, `test-doctor-failed-units.sh`,
`test-doctor-gate-compile-check.sh`, `test-doctor-hotfix.sh`,
`test-doctor-operator-channel.sh`, `test-doctor-orphan-units.sh`,
`test-doctor-snap-fresh.sh`, `test-doctor-store.sh`. None of the 11 carries a `UC-*` token
in its `# covers:` line (audited against every `docs/test-plan/*.toml`), so retiring them
orphans no use case under `plan-matrix`'s rule.

`test-deploy.sh`, `test-deploy-preflight-new-unit.sh`, `test-aeon-chamber-overlay.sh`,
`test-aeon-launch-grammar.sh`, `test-fayth.sh`, `test-freshclone.sh`, `test-pr-stall.sh`,
`test-repo-lanes.sh`, `test-unit-name.sh`, `test-watchtower-failed-units.sh`,
`test-bd-lock-retry.sh`, `bd-pin.sh`, `test-overrides.sh` are **kept**, repointed: each
exercises `doctor` as a fixture dependency for a different subject (deploy's own flow, the
chamber overlay mechanism, fayth dispatch, a fresh clone, etc.), not `doctor`'s own decision
logic, so a fake-backed unit test does not replace what they check.

`install.sh` (`doctor.sh` → `doctor`, bare name unchanged in spirit) and `deploy.sh`
(`_DOCTOR` default, `_activated_release_cmd`'s pinning comment) are repointed in this same
change; see the bead's report for the full caller list, including `release`'s acceptance
harness (`phases.rs`/`tests.rs`'s `tool("doctor.sh")` references) and `conf.sh`'s own
embedded message text (`"run doctor.sh"` → `"run doctor"`, `spira/conf.sh:2248`,
`test-bd-lock-retry.sh`'s matching assertion).

## 7. Fail-closed

Every check that cannot answer at all reports FAIL, not a silent OK: a systemd query that
returns a non-zero exit WITH output (a real fault, distinguished from "no unit file matched
the pattern," which is silent and exit 1 on this systemd — `doctor_check_orphan_units`'s own
comment, carried forward), an unresolvable gate definition, an unreachable store. The
events-substrate probe distinguishes "refused" (a write attempt that errored) from
"discarded" (a write that reported success but never reads back) — two different faults,
two different FAIL messages, exactly as doctor.sh's own comment requires
(law-absence-needs-a-positive-control: a blind detector must say so, not report zero).

## 8. Test strategy

52 unit tests (`cargo test -p doctor`) cover every check's OK/WARN/FAIL branches, including
both `SPIRA_DOCTOR_INSTALLING` softenings and the `SPIRA_OPERATED` fail/warn split. Not
covered here, and not fixable from this side without a live systemd user manager, a real
Dolt server, or a real `gh`/`bd` binary: the literal subprocess argv construction in
`real.rs` (`systemctl --user list-units ...`, `bd -C <db> list --json`, the TCP connect).
That remains the round's integration coverage through the kept, repointed suites (§6),
especially `test-deploy.sh` and `test-deploy-preflight-new-unit.sh` for the two mandatory
caller paths.
