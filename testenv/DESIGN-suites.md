# `testenv suites` — design

The suite population, the gate/timed partition, the per-suite state the harness keeps about
suites (last result). It is a **subcommand family of
the `testenv` binary** (`testenv suites <cmd>`), not a crate of its own: the population, the
suite headers and the result-record format are already testenv's, and a second crate would
carry a second parser of each.

A suite's lifecycle state (active, quarantined, disabled) is not kept here. It lives only in
the test registry; a run is handed the registry's deny list (`SPIRA_TEST_DENY_LIST`) and that
list is the only source of a suite's state. `testenv suites` neither reads nor writes
lifecycle state, and there is no verb to change it.

**Dispatch.** `testenv suites …` when the first argument is exactly `suites`. The runner's
own grammar (`testenv [opts] <branch> [<repo>]`) is untouched; to run the batch on a branch
literally named `suites`, write `testenv -- suites`.

## 1. Intent

1. **Say which suites run nowhere.** The population is the glob `spira/test-*.sh` — never a
   list — and the one hand-written list is the gate's (`gate-suites`). `list`/`names`/
   `status` report the complement, so a suite dropped from the gate becomes visible as
   *timed* rather than invisible.
2. **Render a cheap health block for the watchtower** (`status`): a glob, a read per suite,
   no database, nothing that can hang. Every field that could not be read renders `?`,
   never `0` (law-absence-needs-a-positive-control).

## 2. Contract

### 2.1 Invocation

```
testenv suites [list]                    # default
testenv suites names
testenv suites corpus
testenv suites status
```

Any other subcommand prints `usage: testenv suites [list|names|corpus|status]`
to stderr and exits 2. `observe-flake`, `quarantine`, `unquarantine`, `disable`, `activate`, `lint`,
`hygiene` and `run` are retired and stay unknown subcommands.

### 2.2 Exit codes

| cmd | 0 | 1 | 2 |
|---|---|---|---|
| list, status, corpus | always (an unreadable input renders `?` or is named in the output) | — | — |
| names | printed (empty = nothing to run) | gate list unreadable | — |

### 2.3 Output (callers parse these)

**`list`** — header and one row per suite, `printf '%-26s %-7s %-9s %-8s %s\n'`:
`SUITE RUNS LAST AGE COVERS`. RUNS is `gate`, `timed`, or `?` when the gate list is
unreadable. LAST/AGE are the last-result record's status and age in whole minutes (`<n>m`),
`-` when there is none — and always `-` for a gated suite (its record is not this tool's to
report). COVERS is the `# covers:` value (§3.3). When the gate list is unreadable a blank
line and `<gate-list> is unreadable — which suites the gate runs is unknown` follow.

**`names`** — the timed suites (glob minus gate list), one basename per line, nothing else:
stdout is piped straight into a runner (`testenv suites names | testenv --suites - <br>`).
Diagnostics go to **stderr** (§6 D1).

**`corpus`** — every suite in the population, one per line. Which of them a run skips is the
deny list's say, applied by the runner.

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

### 2.4 Files

`STATE = $SPIRA_SUITES_STATE` (default `$SPIRA_RUN/suites`).

| path | read by | written by |
|---|---|---|
| `<harness>/spira/test-*.sh` | all | — |
| `$SPIRA_GATE_SUITES` (default `$SPIRA_HOME/gate-suites`, else `<harness>/spira/gate-suites`) | list, names, status | — |
| `STATE/<suite>.result` | list, status | nothing any more (§7 F1) |

### 2.5 Environment and configuration

Precedence as testenv's own (§2.5 of DESIGN.md): environment, then spira.toml through the
`spira-config` library, then the default. suites never parses spira.toml itself.

| variable | config key | default |
|---|---|---|
| `SPIRA_RUN` | `spira.run` | derived as conf.sh does (`settings::derive_run`) |
| `SPIRA_SUITES_STATE` | `spira.suites_state` | `$SPIRA_RUN/suites` |
| `SPIRA_SUITES_STALE` | `spira.suites_stale` | 21600 |
| `SPIRA_SUITES_PRIORITY` | `spira.suites_priority` | 3 |
| `SPIRA_GATE_SUITES` | `spira.gate_suites` | `$SPIRA_HOME/gate-suites`, else `<harness>/spira/gate-suites` |
| `SPIRA_TESTENV_HARNESS` | — | the harness root (testenv's `Harness::locate`) |

`<harness>` is the checkout or release root the running binary belongs to.

### 2.6 Callers

| caller | call | reads |
|---|---|---|
| the watchtower | `testenv suites status` | stdout verbatim |
| operator / Ops by hand | `list`, `names`, `corpus` | — |

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
```

### 3.1 Gate list

Per line: drop from the first `#`, trim, skip empty, take the basename. Sorted, as a set.

### 3.2 Priority

The first `# priority: N` line anywhere in the suite (`^# *priority: *`), whitespace removed;
a single digit `0`–`4` is used, anything else falls back to `SPIRA_SUITES_PRIORITY`.

### 3.3 `# covers:`

suite-covers.sh's `suite_covers_of`, exactly: the first line matching `^# *covers: *` gives
the rest of the line; each following line matching `^#[ \t]{2,}[^ \t#]` and not
`^# [A-Za-z][A-Za-z_-]*:` is a continuation, appended after one space with `^#[ \t]+`
stripped; the first non-continuation ends the block. No covers line = empty.

## 5. Collaborators (subprocesses; all behind traits and faked in the tests)

| collaborator | used by | interface |
|---|---|---|
| `host-check.sh --count-undeclared` / `--count-copying` | status | one number on stdout; 30 s wall |

## 6. Decisions

* **D1 — diagnostics never go to a selector's stdout.** `names` with an unreadable gate list
  must not write its refusal into the stream a runner reads as suite names. All diagnostics
  go to stderr. (`status` still prints its one-line `?` form on stdout: that *is* its output.)
* **D5 — host-check has a 30 s wall** in `status`, which the watchtower embeds and which
  must not be another thing that hangs during an outage. A timeout renders `?`.
* **D10 — the population is sorted bytewise,** so `list`/`names`/`corpus` order does not
  change with the caller's locale.

## 6a. spira-lc

**`testenv suites` never touches spira-lc**: no subcommand runs it, reads `SPIRA_LC_BIN` or
sources lc.sh. Pinned by the unit test `suites::tests::suites_never_touches_spira_lc` (the
module sources name none of spira-lc / SPIRA_LC_BIN / lc.sh).

## 7. Findings (not fixed here)

* **F1 — nothing writes `STATE/<suite>.result` any more.** Its producer was the retired
  timed runner. testenv writes the same record format under
  `$SPIRA_BATCH_RESULTS/<key>/`, but many key directories make "newest record per suite"
  a scan, not a read, so `status` cannot switch to it without an index. Either retire the
  timed-result lines or have testenv maintain `STATE/<suite>.result`. An operator decision.
