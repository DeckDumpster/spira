# `testenv suites` — design

The Rust replacement for `spira/suites.sh` (496 lines at 4764d03ec): the suite population,
the gate/timed partition, the per-suite state the harness keeps about suites (last result,
flake observations, clean-run counters, max-age flags), the flake report, quarantine
hygiene and the suite-state lifecycle transitions. It is a **subcommand family of the
`testenv` binary** (`testenv suites <cmd>`), not a crate of its own; §0 says why. This
document is the contract; it was written before the code, from the script's intent and
from every caller, not by porting the script line by line.

## 0. Why a subcommand of testenv, not a `suites` crate

The data is one domain, and testenv already owns half of it:

| datum | testenv (the runner) | suites.sh |
|---|---|---|
| the population (`spira/test-*.sh`) | selects from it, checks `--suites` names against it | globs it |
| `spira/suite-state` (`active/quarantined/disabled`) | reads it at the tree under test (`suite::SuiteStates`) | reads it, and writes it through a branch |
| suite headers (`# requires:`, `# tier:`, …) | parses them (`suite::SuiteHeaders`) | parses `# covers:` and `# priority:` |
| a suite's result record (`<status> <epoch> <secs> <fp> …`) | writes it (`record::ResultRecord`) | reads the same leading four fields |
| flake observations, clean runs, max-age flags | — | owns them |

A separate crate would carry a second parser for `spira/suite-state` and a second reader of
the result-record format — the drift `suite-covers.sh` was written to end ("ONE PARSER, ONE
PLACE", sp-dt8u). As a module (`testenv::suites`) it reuses `suite::SuiteStates`,
`util::iso_utc`, `settings::{Source, derive_run}` and `run::Harness::locate`, and ships in
the binary every caller will already resolve (`spira_bin testenv`). The Ops persona's tool
pattern that testenv's own cutover introduces (`Bash(*testenv *)`) then covers it too.

**Dispatch.** `testenv suites …` when the first argument is exactly `suites`. The runner's
own grammar (`testenv [opts] <branch> [<repo>]`) is untouched; to run the batch on a branch
literally named `suites`, write `testenv -- suites` (the parser already ends options at
`--`). No caller does either today.

## 1. Intent

1. **Say which suites run nowhere.** The population is the glob `spira/test-*.sh` — never a
   list — and the one hand-written list is the gate's (`gate-suites`). `list`/`names`/
   `status` report the complement, so a suite dropped from the gate becomes visible as
   *timed* rather than invisible.
2. **Render a cheap health block for the watchtower** (`status`): a glob, a read per suite,
   no database, nothing that can hang. Every field that could not be read renders `?`,
   never `0` (law-absence-needs-a-positive-control).
3. **Report flakes, never hide them** (`observe-flake`): count distinct runs in which a
   suite flaked within a window; at the threshold file one finding through incident.sh
   (dedupe ref `flake:<suite>`). It never quarantines anything.
4. **Change a suite's lifecycle only through the queue** (`quarantine|disable|activate`):
   the state file is tracked code read from the tree under test, so a change to it is a
   commit on a `spira-suite-state/*` branch, certified and landed by `queue submit`.
5. **Keep hand-placed quarantines honest** (`hygiene`): lift one whose bead has landed and
   whose suite has run clean N times; mail the operator once when one outlives its max age.

## 2. Contract

### 2.1 Invocation

```
testenv suites [list]                    # default
testenv suites names
testenv suites corpus
testenv suites status
testenv suites hygiene
testenv suites lint
testenv suites observe-flake <suite> <run-id>
testenv suites quarantine <suite> <bead> (<reason> | --reason-file <F|->) [--base <rev>]
testenv suites disable    <suite>        (<reason> | --reason-file <F|->) [--base <rev>]
testenv suites activate   <suite>                                         [--base <rev>]
```

`--reason-file` is new: the stdin/file form of a free-text reason (law-payloads-go-on-stdin);
the positional form stays for the callers and runbooks that exist. `--base` is new (§2.4).
An unknown subcommand prints
`usage: testenv suites [list|names|corpus|status|hygiene|lint|observe-flake|quarantine|disable|activate]`
to stderr and exits 2. `suites.sh run` was retired by sp-b99nj and stays retired: it is an
unknown subcommand (exit 2), as it already was.

### 2.2 Exit codes

| cmd | 0 | 1 | 2 |
|---|---|---|---|
| list, status, corpus | always (an unreadable input renders `?` or is named in the output) | — | — |
| names | printed (empty = nothing to run) | gate list unreadable | — |
| hygiene | always | — | — |
| lint | clean, or the lifecycle file is absent (nothing to lint) | any parse/existence/state/reason/bead violation (§6b) | — |
| observe-flake | recorded (filed, below threshold, or filing failed — all say so on stdout) | — | suite or run id missing, no such suite |
| quarantine/disable/activate | branch created, committed and submitted; branch name on stdout | refused under `SPIRA_AEON`; git or queue failure | suite/bead/reason missing or malformed, no such suite |

### 2.3 Output (callers parse these)

**`list`** — header and one row per suite, `printf '%-26s %-12s %-7s %-9s %-8s %s\n'`:
`SUITE STATE RUNS LAST AGE COVERS`. STATE is the suite-state lifecycle (`active`
default). RUNS is `gate`, `timed`, or `?` when the gate list is unreadable. LAST/AGE are the
last-result record's status and age in whole minutes (`<n>m`), `-` when there is none —
and always `-` for a gated suite (its record is not this tool's to report). COVERS is the
`# covers:` value (§3.4). When the gate list is unreadable a blank line and
`<gate-list> is unreadable — which suites the gate runs is unknown` follow.

**`names`** — the timed suites (glob minus gate list), one basename per line, nothing else:
stdout is piped straight into a runner (`testenv suites names | testenv --suites - <br>`).
Diagnostics go to **stderr** (§6 D1).

**`corpus`** — every suite whose state is not `disabled` (active and quarantined), one per
line, from the harness checkout's `spira/suite-state`.

**`status`** — the watchtower's block (watchtower.sh embeds it verbatim):

```
  suites in the tree                  <total>   (<g> gated, <t> timed)
  host suites without # host-reason:  <n|?>
  suites still copying/stubbing (wave 2)<n|?>
  timed suites with no result yet     <n>
  timed results older than <h>h        <n>
  timed suites red at last run        <n>
  timed suites setup/fixture fault at last run<n>
  timed suites skipped at last run    <n>
  oldest timed result                 <m>m   <suite>        | ?   (nothing has run)
```

each line `printf '  %-36s%s\n'` exactly as suites.sh (a label longer than 36 columns runs
into its value, as before). red = `red|timeout|red-unconfirmed`, fault =
`setup-fault|fixture-fault`, skipped = `skip`. The two host-check lines are
`host-check.sh --count-undeclared` / `--count-copying`, `?` when the script is not
executable, fails, prints nothing, or does not answer within 30 s (§6 D5). When the gate
list is unreadable the whole block is the one line
`suites          ?   <gate-list> is unreadable — the gated set is unknown`.

**`observe-flake`** — `observe-flake: <suite>: <n> observation(s) in window (threshold <t>)`;
at or over the threshold, then `observe-flake: <suite> reported (bead: <id>)` or
`observe-flake: <suite> crossed threshold but the finding could not be filed`.

**`hygiene`** — per suite `hygiene: <s> reactivated (bead <b> LANDED, <n> clean runs)` and
`hygiene: mailed operator about <s> (age <n>s)`; last line
`hygiene: <a> reactivated, <m> max-age mailed`.

**`lint`** (§6b, sp-9gd4e; replaces `spira/suite-state.sh`'s `suite_state_lint` +
`suite_state_parse`, deleted) — diagnostics on **stderr**, one per violation:
`suite-state:<n>: not parseable (expected suite|state|since|bead|reason): <line>`,
`suite-state:<n>: suite does not exist: <s>`,
`suite-state:<n>: unknown state <st> (valid: active quarantined disabled)`,
`suite-state:<n>: missing reason for <s>`,
`suite-state:<n>: quarantined suite <s> has no bead id`. On **stdout**, one
`<suite>\t<state>\t<since>\t<bead>\t<reason>` row per known-state entry (skipping
unparseable and unknown-state lines) — `suite_state_parse`'s exact shape, read by
`suite-state-fence.sh`. An absent lifecycle file prints nothing and exits 0 (every suite
reads as active; there is nothing to lint, the same case `list`/`corpus`/`hygiene` treat as
empty rather than a fault).

**Transitions** — stdout is exactly the branch name; queue's own output goes to stderr (as
`queue.sh submit … >&2` did). Refusals, on stderr:
`suites <state>: aeons may not write suite-state transitions; submit a branch from an operator or Ops session`,
`suites <state>: suite name required`, `suites <state>: no such suite: <s>`,
`suites quarantine: bead id required`, `suites <state>: reason required`,
`suites <state>: cannot create branch <b>`, `suites <state>: commit failed`,
`suites <state>: queue submit failed for <b> — branch exists but is not certified`.

### 2.4 Files, refs and rows

`STATE = $SPIRA_SUITES_STATE` (default `$SPIRA_RUN/suites`).

| path | read by | written by |
|---|---|---|
| `<harness>/spira/test-*.sh` | all | — |
| `$SPIRA_GATE_SUITES` (default `$SPIRA_HOME/gate-suites`, else `<harness>/spira/gate-suites`) | list, names, status | — |
| `<harness>/$SPIRA_SUITE_STATE_FILE` (default `spira/suite-state`) | list, corpus, hygiene | — (never the working tree; §6 D3) |
| `STATE/<suite>.result` | list, status | nothing any more (§7 F1) |
| `STATE/<suite>.flakeobs` | observe-flake | observe-flake (append) |
| `STATE/<suite>.clean-runs` | hygiene | hygiene (removed on reactivation); no producer (§7 F2) |
| `STATE/<suite>.maxage-mailed` | hygiene | hygiene (created after a sent mail; removed on reactivation) |
| `$LANDSTATE/<bead>` (default `$SPIRA_RUN/landstate`) | hygiene | — (lib.sh `land_mark`) |
| home repo: `refs/heads/spira-suite-state/<suite-sans-.sh>-<YYYYmmddTHHMMSSZ>` | — | transitions (created, never moved) |
| home repo objects: one blob, one tree, one commit | — | transitions |

Transitions build the commit **without a checkout** (§6 D2): the file at `<base>` is read
with `git show`, rewritten (§3.3), and committed with `hash-object -w --stdin`, a temporary
index (`read-tree <base>`, `update-index --cacheinfo`), `write-tree`, `commit-tree -F -`
(message on stdin) and `update-ref <branch> <commit> ""` (create-only). No working tree,
index or HEAD of any checkout is touched. `<base>` defaults to the home repository's landing
ref (`spira_landref`), else its `HEAD`. Commit identity: `SPIRA_GIT_NAME`/`SPIRA_GIT_EMAIL`,
default `spira`/`spira@spira.invalid`; message `suite-state: <suite> -> <state>  sp-emvlk`.

### 2.5 Environment and configuration

Precedence as testenv's own (§2.5 of DESIGN.md): environment, then spira.toml through the
`spira-config` library, then the default. suites never parses spira.toml itself.

| variable | config key | default |
|---|---|---|
| `SPIRA_RUN` | `spira.run` | derived as conf.sh does (`settings::derive_run`) |
| `SPIRA_SUITES_STATE` | `spira.suites_state` | `$SPIRA_RUN/suites` |
| `SPIRA_SUITES_STALE` | `spira.suites_stale` | 21600 |
| `SPIRA_SUITES_PRIORITY` | `spira.suites_priority` | 3 (conf.sh's; suites.sh's own `2` was always overridden by conf.sh) |
| `SPIRA_GATE_SUITES` | `spira.gate_suites` | `$SPIRA_HOME/gate-suites`, else `<harness>/spira/gate-suites` |
| `SPIRA_SUITE_STATE_FILE` | `spira.suite_state_file` | `spira/suite-state` |
| `SPIRA_FLAKE_QUARANTINE_AT` | `spira.flake_quarantine_at` | 2 |
| `SPIRA_FLAKE_WINDOW` | `spira.flake_window` | 604800 |
| `SPIRA_QUARANTINE_CLEAN_RUNS` | `spira.quarantine_clean_runs` | 10 |
| `SPIRA_QUARANTINE_MAX_AGE` | `spira.quarantine_max_age` | 604800 |
| `SPIRA_INCIDENT` | — | `incident.sh` on PATH (sp-gypjk) |
| `SPIRA_MAIL_CMD` (new) | — | `mail.sh` on PATH (sp-gypjk) |
| `LANDSTATE` | — | `$SPIRA_RUN/landstate` |
| `SPIRA_AEON` | — | non-empty refuses transitions |
| `SPIRA_GIT_NAME`, `SPIRA_GIT_EMAIL` | — | `spira`, `spira@spira.invalid` |
| `SPIRA_TESTENV_HARNESS` | — | the harness root (testenv's `Harness::locate`) |

`<harness>` is the checkout or release root the running binary belongs to — the same
directory suites.sh called `$HERE/..`. `SPIRA_HOME_REPO`, `SPIRA_SCOPE_LABEL` and `SPIRA_DB`
come from the one lib.sh seam (§4), because conf.sh derives them in ways no library shares.

### 2.6 Callers

| caller | call | reads |
|---|---|---|
| `spira/watchtower.sh:966-970` (spira-watchtower.timer) | `bash "$SUITES" status` (`SPIRA_SUITES_SH` override for tests) | stdout verbatim |
| `spira/gate-check.sh:147` (spira-gate-check.timer) | `bash "$HERE/suites.sh" observe-flake "$_suite" "$_run_id"` | nothing (`|| true`) |
| `spira/verdict.sh:1719` (spira-verdict.service) | `bash "$HERE/suites.sh" observe-flake "<suite>" "$batch_head"` | nothing |
| `spira/aeon.sh:2073` | `{{SUITES}}` → `$SPIRA_HOME/suites.sh` in persona prompts | — |
| `spira/chamber/ops.md:8,55` | tells Ops to run `{{SUITES}} run` (retired; §7 F4) | — |
| `spira/chamber/ops.fayth:165`, prod `persona.ops` tools | `Bash(*suites.sh*)` | — |
| operator / Ops by hand | `list`, `names`, `corpus`, `quarantine`, `disable`, `activate`, `hygiene` | — |

No systemd unit or workflow invokes suites.sh directly. `hygiene` and `corpus` have no
automated caller (§7 F3).

## 3. Schema

```rust
/// The population: basenames matching test-*.sh in <harness>/spira, sorted.
struct Population(Vec<String>);

/// The gate's list: basenames, `#` comments and blank lines dropped. Err = unreadable,
/// which every reader must treat as UNKNOWN, never as "the gate runs nothing".
type GateList = Result<BTreeSet<String>, Unreadable>;

enum Runs { Gate, Timed, Unknown }

/// STATE/<suite>.result — the leading fields of testenv's ResultRecord:
/// `<status> <epoch> <secs> <fp>`; status non-empty and epoch all digits, else absent.
struct LastResult { status: String, at: u64, secs: String, fingerprint: String }

/// STATE/<suite>.flakeobs — one `<epoch> <run_id>` per line, append-only.
struct FlakeObs { at: u64, run_id: String }
/// The count that matters: distinct run ids with at >= now - window.

/// STATE/<suite>.clean-runs — one integer; unreadable/malformed = 0.
struct CleanRuns(u64);

/// A spira/suite-state row (testenv::suite::SuiteStateRow, shared with the runner):
/// `<suite> | <state> | <since UTC> | <bead> | <reason>`.
struct SuiteStateRow { suite, state: SuiteState, since, bead, reason }

enum Transition { Quarantine { bead, reason }, Disable { reason }, Activate }

/// $LANDSTATE/<id> — "<STATE> <tip|none> <epoch> [reason…]" (lib.sh land_mark).
struct LandState { state: String, tip: String, at: u64 }

/// What the lib.sh seam answers (§4).
struct Conf { home_repo: String, scope_label: Option<String>, db: String,
              repo_path: Option<PathBuf>, landref: Option<String> }

/// The finding observe-flake files: incident.sh's env interface plus a stdin payload.
struct FlakeFiling { suite, count: u64, window: u64, priority: u8, labels: String,
                     repo: String, db: String, path: PathBuf }
```

### 3.1 Gate list

Per line: drop from the first `#`, trim, skip empty, take the basename. Sorted, as a set.

### 3.2 Priority

The first `# priority: N` line anywhere in the suite (`^# *priority: *`), whitespace removed;
a single digit `0`–`4` is used, anything else falls back to `SPIRA_SUITES_PRIORITY`.

### 3.3 Rewriting spira/suite-state

Every line is kept except those whose first `|`-field (after dropping any `#` comment and
trimming) equals the suite; blank and comment-only lines are kept verbatim. Every kept line
ends in `\n`. A non-active transition then appends
`<suite> | <state> | <YYYY-mm-ddTHH:MM:SSZ> | <bead> | <reason>\n`; `activate` appends
nothing. **New validation** (§6 D4): suite, bead and reason must be one line and contain no
`#` (the reader drops everything after it) and suite/bead no `|`.

### 3.4 `# covers:`

suite-covers.sh's `suite_covers_of`, exactly: the first line matching `^# *covers: *` gives
the rest of the line; each following line matching `^#[ \t]{2,}[^ \t#]` and not
`^# [A-Za-z][A-Za-z_-]*:` is a continuation, appended after one space with `^#[ \t]+`
stripped; the first non-continuation ends the block. No covers line = empty.

### 3.5 Flake payload (stdin of `incident.sh file "why does <s> fail intermittently" -`)

```
<s> has <n> flake observation(s) within the <w>s window. It is filed and nothing is
quarantined: the suite keeps running in every batch, so a repeat failure still reaches the
gate instead of being hidden.

  suite            <s>
  observations     <n> in <w>s
  reproduce        bash spira/<s>

The dedupe ref is flake:<s> — a later observation bumps recurrence on this bead rather than
filing another.
```

with `SPIRA_INCIDENT_TYPE=bug SPIRA_INCIDENT_PRIORITY=<§3.2> SPIRA_INCIDENT_ACTOR=suites
SPIRA_INCIDENT_LABELS=[<scope>,]plan SPIRA_INCIDENT_REPO=<home_repo>
SPIRA_INCIDENT_REF=flake:<s> SPIRA_SIN_EXEMPT=1 SPIRA_INCIDENT_PATH=<harness>/spira/<s>
SPIRA_INCIDENT_CAUSE=suite-flaky SPIRA_DB=<db>`. The id is the last stdout line with
whitespace removed; it must be `[A-Za-z0-9-]+` not starting or ending with `-`.

## 4. The lib.sh seam (one)

| seam | what | why not Rust |
|---|---|---|
| S1 `conf` | after sourcing `lib.sh` (and so conf.sh): `SPIRA_HOME_REPO`, whether `SPIRA_SCOPE_LABEL` is set and its value, `SPIRA_DB`, `repo_root "$SPIRA_HOME_REPO"` (+ its status), `spira_landref <that path>` | conf.sh derives the home repo from the git common dir, a MANIFEST or the repo map, and `SPIRA_SCOPE_LABEL`/`SPIRA_DB` with set-but-empty semantics; `repo_root`/`spira_landref` are the repo-map resolver every component shares. A second derivation is the drift law-config-through-the-cli-only names. |

Mechanism (the queue crate's): `bash` with **no arguments**; stdin is a fixed script
(compiled in) followed by one NUL-terminated value, the directory holding `lib.sh`. The
script reads its value, detaches stdin, sources lib.sh, and prints `\x1e` then
`key=value\0` records; anything lib.sh logs before the mark is passed to stderr. Nothing is
in argv or the environment. Called only by the paths that need it — a flake **filing** and
a **transition** — never by `list`, `names`, `corpus` or `status`.

**Not seams:** `log` (reimplemented: `<ISO> spira: <msg>`), the suite-state parser (shared
with the runner), the landstate read (a documented line format), git (subprocess),
incident.sh, mail.sh, host-check.sh and the queue (whole programs, §5).

## 5. Collaborators (subprocesses; all behind traits and faked in the tests)

| collaborator | used by | interface |
|---|---|---|
| `git` | transitions | `rev-parse`, `cat-file -e`, `show`, `hash-object -w --stdin`, `read-tree`, `update-index --cacheinfo`, `write-tree`, `commit-tree -F -`, `update-ref` |
| `incident.sh file <title> -` | observe-flake | env (§3.5), payload on stdin, id on the last stdout line |
| `mail.sh send operator --from … --subject … [--bead …]` | hygiene | body on stdin |
| `host-check.sh --count-undeclared` / `--count-copying` | status | one number on stdout; 30 s wall |
| `queue submit <branch>` | transitions | `queue submit`, by name on the launcher's PATH (sp-gypjk; formerly `$SPIRA_QUEUE_BIN`, a sibling of this binary, or `queue.sh`); stdout+stderr to our stderr |

## 6. Decisions (behaviour deliberately changed)

* **D1 — diagnostics never go to a selector's stdout.** suites.sh's `log` printed to stdout,
  so `names` with an unreadable gate list wrote its refusal into the stream a runner reads
  as suite names. All diagnostics go to stderr. (`status` still prints its one-line `?`
  form on stdout: that *is* its output.)
* **D2 — a transition never checks anything out.** suites.sh ran `git checkout -b` in the
  harness checkout (the production checkout, for Ops) and on success never switched back,
  leaving that checkout's HEAD on `spira-suite-state/*` (test-queue-submit.sh runs
  `git checkout -q main` before every transition to undo exactly this). The commit is now built with
  plumbing on `<base>` (default: the landing ref, not whatever HEAD happened to be) in the
  home repository — the one `queue submit` resolves, so the branch is where the queue
  looks for it. `--base` overrides.
* **D3 — hygiene lifts a quarantine through the queue.** suites.sh's reactivation rewrote
  the working-tree `spira/suite-state` in place, uncommitted: invisible to the gate (which
  reads the tree under test) and a dirty production checkout. It now runs the `activate`
  transition (branch + `queue submit`), and only on success clears the counters and mails.
* **D4 — hygiene reads LANDED from landstate, not a bd label.** suites.sh asked
  `bd show` for a label `land_state:LANDED`; nothing in the harness writes that label (the
  only bead carrying it is a leaked test fixture, sp-8h8dd), so reactivation could never
  fire. Land state lives in `$LANDSTATE/<id>` (lib.sh `land_mark`), whose first field is
  read. hygiene no longer needs bd at all.
* **D5 — host-check has a 30 s wall** in `status`, which the watchtower embeds and which
  must not be another thing that hangs during an outage. A timeout renders `?`.
* **D6 — transitions validate what the file format cannot carry** (§3.3): a `#` or newline
  in a reason silently truncated the row on read; `|` in a suite or bead shifted fields.
  Refused with exit 2. `--reason-file F|-` added (law-payloads-go-on-stdin).
* **D7 — a suite name is a basename.** `observe-flake` and transitions refuse a name with
  `/` or a leading `.`, since it is joined into STATE paths and a tree path. A run id's
  internal whitespace is folded to `_` so one observation stays one line.
* **D8 — suite existence for a transition is checked in `<base>`'s tree** (`spira/<suite>`),
  the tree the commit is made on, rather than in whatever checkout the tool runs from.
* **D9 — filing diagnostics are visible.** file_flake's `log` lines went into the
  `$(...)` that captured the bead id and were discarded; they go to stderr.
* **D10 — the population is sorted bytewise.** suites.sh piped the glob through `sort`,
  which collates by the caller's locale (en_US puts `test-acceptance-local-stop.sh` before
  `test-acceptance-local.sh`; C does the reverse), so `list`/`names`/`corpus` order changed
  with whoever ran it. Byte order is deterministic. The sets are identical (verified, §8).

## 6b. `lint` retires `spira/suite-state.sh` (sp-9gd4e)

`suite-state.sh` was a sourced-only library (never executed, per its own header) with two
remaining callers: `test-suite-state.sh` (its own test suite) and `suite-state-fence.sh` (a
gate fence). Everything else that once needed it — the read side (`suite_state_of`) and the
write side (`suite_state_write`) — was already ported natively to this crate by an earlier
bead: `crate::suite::SuiteStates` (read) and `model::rewrite_state` (write, behind the
`quarantine`/`disable`/`activate` transitions above). Only `suite_state_lint` and
`suite_state_parse` had no Rust equivalent; `lint` (§2.3) is both, merged into one pass, so
`suite-state-fence.sh` gets one command instead of two bash functions. Both bash files are
deleted; `test-suite-state.sh`'s cases are subsumed by `suites::tests::lint_*` and
`suite::tests::suite_state_*` (already covered `suite_state_of`/`suite_state_write` before
this bead). `suite-state-fence.sh` itself is not rewritten to Rust here — only its call site
changes (source the deleted bash → run `testenv suites lint`) — its own bd/CLOSED-bead check
is outside this bead's remit (suite-state.sh never had it).

## 6a. lifecycle_enforce (operator decision, 2026-09-28)

`lifecycle_enforce` is the single switch for everything touching spira-lc (env
`SPIRA_LIFECYCLE_ENFORCE` `1`/`true`, else `spira.lifecycle_enforce`, else OFF).
**`testenv suites` never touches spira-lc in either mode**: no subcommand runs it, reads
`SPIRA_LC_BIN`, sources lc.sh, or reads the switch, so OFF and ON behave identically here.
The one lifecycle fact it reads — whether a quarantine's bead LANDED (hygiene, D4) — comes
from `$LANDSTATE/<id>`, which lib.sh `land_mark` writes whichever way the switch is set
(queue land-local calls it in both modes). A transition's `queue submit` is the queue
binary's own business, and the queue crate applies the switch there. Pinned by the unit
test `suites::tests::suites_never_touches_spira_lc_in_either_lifecycle_mode` (the module
sources name none of spira-lc / SPIRA_LC_BIN / lc.sh / the switch; settings resolve
identically with the switch at 0 and 1; hygiene's LANDED decision is the landstate
file's).

## 7. Findings (not fixed here)

* **F1 — nothing writes `STATE/<suite>.result` any more.** Its producer was the retired
  timed runner (`suites.sh run`, sp-b99nj). In production the newest of 336 `.result`
  files is from 2026-09-17, so every timed-result field in the watchtower's block is frozen
  debris or "no result yet". testenv writes the same record format under
  `$SPIRA_BATCH_RESULTS/<key>/`, but 3,195 key directories make "newest record per suite"
  a scan, not a read, so `status` cannot switch to it without an index. Either retire the
  timed-result lines or have testenv maintain `STATE/<suite>.result` (latest record per
  suite for a run of the landing ref). An operator decision; unchanged here.
* **F2 — clean-run counters have no producer** (`cleanruns_inc` was the timed runner's), so
  reactivation still cannot fire after D4 unless a counter is written by hand. The natural
  producer is a run of the landing ref in which a quarantined suite passed.
* **F3 — `hygiene` and `corpus` have no automated caller.** gate-spira.sh:235 and
  suite-state-fence.sh:6 describe hygiene as if it ran.
* **F4 — the Ops prompt instructs a retired command.** spira/chamber/ops.md:55 tells every
  Ops aeon to run `{{SUITES}} run` "before you start diagnosing"; it exits 2 with a usage
  line. The Cutover drops that step.

## 8. Verification

* `cargo test -p testenv`: 133 passed (45 in `suites::`), `cargo clippy -p testenv
  --all-targets` clean, `cargo build --workspace` clean.
* Read-only parity against `bash spira/suites.sh` on this worktree (same
  `SPIRA_SUITES_STATE`): `status` byte-identical (559 suites, 20 gated, host-check counts
  497/150); `names` (539), `corpus` (559) and `list` identical as sets, differing only in
  order (D10).
* `testenv suites run` → usage, exit 2; `observe-flake` with no suite → exit 2;
  `SPIRA_AEON=x … quarantine` → the aeon refusal, exit 1.
* Transitions were exercised only against a scratch git repository (real git plumbing,
  `suites::real::tests::commit_file_builds_a_branch_without_touching_the_checkout`) and
  fakes; no queue submit was run.

## 9. Cutover (for the Concierge — no bash, unit or workflow edited here)

Line numbers against 4764d03ec. `SPIRA_TESTENV_BIN` is new, resolved like its neighbours.

| # | file:line | current | replacement |
|---|---|---|---|
| 1 | `spira/conf.sh` after `:1541` | — | `: "${SPIRA_TESTENV_BIN:=$(spira_bin testenv 2>/dev/null)}"` and add `SPIRA_TESTENV_BIN` to the key list at `:102` beside `SPIRA_BATCHER_BIN` |
| 2 | `spira/watchtower.sh:966-970` | `SUITES="${SPIRA_SUITES_SH:-$(dirname "$0")/suites.sh}"` … `if [ -r "$SUITES" ]; then suites_block="$(bash "$SUITES" status 2>/dev/null)"` | `if [ -n "${SPIRA_SUITES_SH:-}" ]; then suites_block="$(bash "$SPIRA_SUITES_SH" status 2>/dev/null)"; elif [ -x "${SPIRA_TESTENV_BIN:-}" ]; then suites_block="$("$SPIRA_TESTENV_BIN" suites status 2>/dev/null)"; else suites_block="  (unavailable — the testenv binary is missing, so nothing knows which suites run nowhere)"; fi` then the existing empty-output line with `testenv suites status produced nothing` |
| 3 | `spira/gate-check.sh:147` | `bash "$HERE/suites.sh" observe-flake "$_suite" "$_run_id" 2>/dev/null \|\| true` | `"$SPIRA_TESTENV_BIN" suites observe-flake "$_suite" "$_run_id" 2>/dev/null \|\| true` |
| 4 | `spira/verdict.sh:1719` | `bash "$HERE/suites.sh" observe-flake "${_line#flaky: }" "$batch_head" \` | `"$SPIRA_TESTENV_BIN" suites observe-flake "${_line#flaky: }" "$batch_head" \` |
| 5 | `spira/aeon.sh:2073` | `-e "s\|{{SUITES}}\|$SPIRA_HOME/suites.sh\|g"` | `-e "s\|{{SUITES}}\|$SPIRA_TESTENV_BIN suites\|g"` |
| 6 | `spira/chamber/ops.md:8` | "`{{SUITES}}` below all resolve to scripts under it" | "`{{SUITES}}` (the `testenv suites` tool) … resolve to tools under it" |
| 7 | `spira/chamber/ops.md:49-66` | step 2 running `{{SUITES}} run` | replace the command with `{{SUITES}} status` and the paragraph with: "It prints which suites run nowhere and what each last said; name the timed suites it reports in your close. A red there is ordinary work for whoever can change the code." (drop the "budgeted to fit inside your wall" text; nothing runs) |
| 8 | `spira/chamber/ops.fayth:165` | `Bash(*suites.sh*),Bash(*testenv-batch.sh*)` | `Bash(*testenv *)` (testenv's own cutover makes the same change for testenv-batch.sh) |
| 9 | prod `~/.config/spira/spira.toml` `persona.ops` tools (`"Bash(*suites.sh*)"`, line 225) | — | `"Bash(*testenv *)"`, through `spira-config` (law-config-through-the-cli-only) |
| 10 | `spira/test-lint-chamber.sh:127` | `-e "s\|{{SUITES}}\|$HERE/suites.sh\|g"` | `-e "s\|{{SUITES}}\|testenv suites\|g"` |
| 11 | `spira/conf.sh:1750,1764-1769,1832-1836`, `spira.conf.example:271-293`, `README.md:321,467`, `CLAUDE.md:48,59-61`, `spira/gate-suites` header, `spira/gate-spira.sh:235,374`, `spira/suite-state-fence.sh:6`, `spira/suite-covers.sh:3-12,73`, `spira/tap-jsonl.sh:5,12`, `spira/suite-assert.sh:5-10`, `spira/aeon.sh:1733`, `spira/test-lifecycle-guard.sh:7`, `spira/test-test-plan-crate.sh:9` | comments naming `suites.sh` | name `testenv suites` (comments only) |
| 12 | **delete** `spira/suites.sh` | — | replaced by `testenv suites` |
| 13 | `spira/suite-state.sh` | **done (sp-9gd4e).** `suites.sh` and `testenv-batch.sh` are both already gone; the one remaining blocker, `suite-state-fence.sh`'s `suite_state_lint`/`suite_state_parse`, is now `testenv suites lint` (§2.3, §6b). File deleted, along with `test-suite-state.sh` (§6b). |

Tests (bash; none run here):

| suite | action |
|---|---|
| `test-suites-hygiene.sh`, `test-suites-flake-report.sh`, `test-suites-stale-unreached.sh` | **retire** — covered by `cargo test -p testenv suites::` (flake window/dedupe/threshold/filing, hygiene reactivation/max-age/once-only, stale debris ignored) |
| `test-queue-submit.sh:226-290` (transition → queue submit) | **repoint** to `"$SPIRA_TESTENV_BIN" suites quarantine …` with `SPIRA_QUEUE_BIN` pointing at its queue stub; assert the branch exists in the fixture repo **and the fixture checkout's HEAD did not move** (D2) |
| `test-guards.sh:256` | the rendered-brief allow case `"${FAKE_PROD}/suites.sh status"` becomes `"${FAKE_PROD_BIN}/testenv suites status"` (whatever path row 5 renders) |
| `test-guards.sh:609-616` | repoint the aeon refusal to `SPIRA_AEON=x "$SPIRA_TESTENV_BIN" suites quarantine …` (message unchanged) |
| `test-verdict.sh:254-259`, `test-verdict-replay.sh:100-104`, `test-eject-unattributed.sh:124-128` | the `$SH/suites.sh` stub becomes a `SPIRA_TESTENV_BIN` stub that logs `$*` (the log now starts `suites observe-flake …`) |
| `test-watchtower*.sh`, `test-snap-stale-threshold.sh` | unchanged: they inject `SPIRA_SUITES_SH`, which row 2 keeps as the test seam |
| `test-config-compat-master-base.sh:182`, `test-queue-step-eject-race.sh:66`, `test-install-hooks-artifact.sh:126` | drop `suites.sh` from the stubbed-script lists |
| `test-suite-state.sh` | **retired (sp-9gd4e)** — tested `suite-state.sh`, deleted; covered by `cargo test -p testenv suite::` and `suites::lint_*` |
