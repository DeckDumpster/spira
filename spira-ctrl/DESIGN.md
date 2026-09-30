# spira-ctrl — DESIGN.md

Rust-rewrite wave 5e (sp-6onps). Replaces `spira/ctrl.sh` (bash + python3).

## Intent

A control operation — suspending a timer, pausing a persona, draining a lane — is not a
source-code change, and must survive `git pull`, `git reset`, and `install.sh`. It needs a
place to live that git does not touch: `$SPIRA_CTRL`, a JSON file in the gitignored runtime
directory. Every entry must carry a reason and an owning bead, so "why is this not running"
has an answer inside the same record that says it is not running, and so a suspension has a
declared owner rather than becoming permanent by accident.

`ctrl.sh divergence` exists because the control plane only *records* intent — it never
reconciles. A tool that re-applies its own intent on a timer hides the same class of fault
in the other direction (the "operator disables a unit by hand, automation re-enables it"
loop this mechanism replaced). Divergence reports two shapes of mismatch instead: a declared
suspension whose unit is running anyway, and a masked unit with no declared reason.

## Contract

- Storage: one JSON object at `$SPIRA_CTRL`, keyed by subject; each value keyed by operation
  ("suspend" is the only one implemented); each leaf: `reason`, `owner`, `when`, `by`. Byte-
  compatible with the file `ctrl.sh` (bash) wrote and read — this crate does not migrate or
  reshape it.
- `suspend <subject> --reason <text> --owner <bead>`: both flags required; refuses otherwise.
- `resume <subject>`: drops the subject's `suspend` entry, and the subject entirely once it
  carries no other ops.
- `check <subject>`: exit 0 if suspended, 1 if not — silent, for use in `&&`/`||`.
- `reason <subject>`: the suspension's reason, or nothing.
- `list`: every entry, human-formatted, sorted by subject then op.
- `suspended`: every entry as `subject\treason`, one per line — the bulk-read replacement
  for what `CTRL_LIB=1 . ctrl.sh; ctrl_load_suspended` used to give a bash caller (see
  Decisions).
- `divergence`: reports both directions above; exit 1 if any found, 2 if the control file
  cannot be read, 0 if clean.

## Schema

```
CtrlData = BTreeMap<subject: String, Ops>
Ops      = BTreeMap<op: String, BTreeMap<field: String, String>>
```

`BTreeMap` at both levels, not a struct wrapped in `serde_json::Value`'s default map — so
JSON serialization sorts keys the same way Python's `json.dumps(..., sort_keys=True)` did,
without depending on `serde_json`'s `preserve_order` feature being off. `lib.rs`'s
`json_round_trips_and_sorts_keys_like_python_sort_keys` test pins this.

## Decisions

- **`ctrl.sh` is the `ctrl` binary; `ctrl.sh` is a release-packaging symlink to it.** A
  Cargo `[[bin]]` target's name becomes its crate name, and a dot is not a legal crate-name
  character — `ctrl.sh` cannot be the real binary's name. The fleet convention (every other
  rewritten tool dropped its `.sh` suffix: `gate-check`, `landing-pass`, …) says drop it
  here too; `build-tarball.sh` then adds a symlink so any caller still spelling the old name
  — a test suite, a doc, a script not yet repointed — keeps working. `world.sh` and
  `slay.sh` keep bare `.sh` names for a stronger reason (see `spira-world`'s DESIGN.md); this
  crate gets the same symlink mechanically, as a courtesy to whatever still names it that way.

- **`suspended` is a new subcommand, not a bash-library replacement.** `install.sh` and
  `watchtower.sh` used to do `CTRL_LIB=1 . ctrl.sh` to load `ctrl_load_suspended` and
  `ctrl_is_suspended` as bash functions, reading the control file once per process instead
  of once per unit checked (`sp-rnps9`). Sourcing a compiled binary is not a thing, so both
  callers now run `ctrl suspended` once and build the same associative array from its output
  — `ctrl_is_suspended` itself is unchanged, a three-line pure lookup, now defined locally in
  each caller instead of sourced. This is a call-site edit in those two scripts (not new
  logic: the function's body is identical to `ctrl.sh`'s own), covered by this crate's
  `load_suspended` function and its own tests, not by a new suite.

- **`divergence`'s systemctl reads stay untested at the integration level in this crate.**
  The pure parts — which unit-name forms a subject expands to, how a masked unit's filename
  maps back to a subject — are unit-tested (`unit_forms`, `subject_of_masked_unit`). The
  systemctl-calling shell around them is exercised by `test-ctrl.sh`'s existing divergence
  cases (unchanged: a black-box CLI suite with `$SPIRA_SYSTEMCTL` stubbed, which needed no
  edit to keep validating this binary instead of the bash it replaced).

- **`today()` shells to `date(1)` rather than carrying a timezone database.** `ctrl.sh
  suspend`'s own `when` field is computed the same way (`TZ=... date '+%Y-%m-%d'`); no crate
  in this workspace depends on `chrono` or a TZ database, and one call's worth of date
  formatting does not justify adding one.

## Parity

`ctrl.sh`'s own suite, `test-ctrl.sh`, is unchanged — it drives the CLI as a subprocess with
`$SPIRA_CTRL` pinned to a temp file and `$SPIRA_SYSTEMCTL` stubbed, and never sourced the
bash file as a library, so it now validates `ctrl` (via the `ctrl.sh` symlink) exactly as it
validated the bash version. Manually verified end to end (suspend → list → check → reason →
resume → list, and divergence with an empty control file) against the built binary; see the
delivery report for the transcript.
