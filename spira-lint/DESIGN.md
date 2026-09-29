# spira-lint — design

One binary replaces the harness's bash fences one rule at a time. This document is the
specification each rule is implemented from; the bash scripts were read for their intent
and their test cases, not transcribed. Where a bash fence's heuristic was wrong, this says
so and says what the rule does instead.

## The program

```
spira-lint [--root <dir>] [--only <rule>]
```

- `--root` defaults to the git work tree containing the current directory.
- `--only <rule>` runs one rule; an unknown name is a usage error.
- **One walk.** `git ls-files -z` (tracked) and `git ls-files -z --others --exclude-standard`
  (untracked, not ignored), once. Each rule filters that walk; no rule lists files itself.
  A file's bytes are read at most once and shared between rules.
- **Output.** One line per finding on stdout, `<rule>: <path>[:<line>]: <message>`, sorted by
  rule order, then path, then line. After a rule's findings, its hint (what to do about it)
  goes to stderr once. A clean rule prints nothing; a clean run prints one summary line to
  stderr.
- **Exit.** `0` clean; `1` any finding; `2` usage; `3` a rule refused to report clean (a bad
  root, an empty scope, an unreadable or malformed allow file). A refusal is never a pass:
  an empty scope is indistinguishable from a matcher that never fires
  (law-absence-needs-a-positive-control).

### Shared schema

```rust
pub struct Entry { pub path: String, pub tracked: bool }   // one file in the walk

pub struct Finding {
    pub rule: &'static str,     // the Rule's name
    pub path: String,           // repo-relative
    pub line: Option<usize>,    // 1-based, when the finding has one
    pub message: String,
}

pub enum LintError {
    NotARepo(String),                                   // the walk could not run
    EmptyScope,                                         // nothing to check: refuse
    BadAllow { file: String, line: usize, reason: String },
}

pub trait Rule {
    fn name(&self) -> &'static str;
    fn allow_file(&self) -> Option<&'static str>;       // repo-relative, shrink-only
    fn applies_to(&self, e: &Entry) -> bool;            // the scope
    fn check(&self, tree: &Tree) -> Result<Vec<Finding>, LintError>;
    fn hint(&self) -> &'static str;
}
```

**Allow lists are shrink-only.** Each rule's allow file is the same file its bash fence
read, in the same format, so nothing migrates. A line leaves the list when the work that
fixes it lands. Nothing is added for a newly written file, because a new file has no debt
to inherit. The program cannot see history, so it cannot refuse growth by itself. What it
can refuse is an entry that names a file which no longer exists, and `fence-scripts` does
(below).

**Allow files share one syntax.** A line is an entry unless it is blank or its first
non-blank character is `#`.

**Parsing, not grepping, where the construct matters.** Two small lexers back the rules:

- `lex::shell`: a bash lexer good enough to recover *simple commands*. It handles quoting
  (`'…'`, `"…"`, `$'…'`), backslash-newline continuation, comments, `$(…)`, backticks,
  `<(…)`, `${…}`, `$((…))`, redirections, and heredocs, whose bodies are data and are
  skipped. For each command it yields the words (the raw text, the start line, and the
  parameters the word itself expands, excluding nested substitutions) and the redirections.
  Nested substitutions yield their own commands.
- `lex::rust`: classifies every byte of a Rust source as code, comment or literal. It
  handles `//`, nested `/* */`, strings, raw strings with any number of `#`, byte strings,
  and char literals as distinct from lifetimes.

Known limits of `lex::shell`, all accepted. A `case` pattern's `)` inside `$(…)` is tracked
by counting `case`/`esac`. A pattern written with a leading `(` is not. Neither the
`bash -c '…'` nor the `eval` string is parsed as code.

---

## Rule `config-fence`

Ported from `spira/config-fence.sh` (sp-a8gna).

**Intent.** `spira.toml` is a multi-tenant config store, and `repo-map` duplicates part of it
by hand. `spira-config`, the crate and its CLI, is the only thing that may find, parse or
write either. A second reader or writer is a second schema that drifts.

**Scope.** Tracked and untracked-not-ignored files matching the git pathspecs
`spira/*.sh`, `spira/gate-suites`, `spira/chamber/*.fayth`, `spira/chamber/*.md` or `*.rs`
(`*` crosses `/`, as git's default pathspec does), minus anything under `spira-config/`.

**Violations**, reported as one finding per file whose message lists the kinds
(`name,parse,write`):

| kind | what | how it is recognised |
|---|---|---|
| `name` | the literal `spira.toml` or `repo-map` anywhere, comments included | substring. Every occurrence this fence exists for was in a comment, so this one stays textual on purpose |
| `parse` | a TOML parser runs in a file that also has `name` | `.rs`: a path into the `toml`, `toml_edit` or `basic_toml` crate (`toml::…`) in **code**, meaning not in a comment, not in a string, and not a sub-module path like `spira_config::toml::`. Other files: the Python `tomllib` token |
| `write` | the config path is written | `.sh`: a command, recovered by `lex::shell`, that writes a word expanding `$SPIRA_TOML` or `$SPIRA_REPO_MAP`. That is a `>`/`>>`/`>|`/`<>`/`&>`/`&>>` redirection target, any argument of `sed -i…`/`--in-place` or `tee`, or the destination (last argument) of `cp`/`mv`/`install`/`ln`. `.rs`: a call to `fs::write`/`File::create`/`File::create_new` whose first argument, or to `fs::rename`/`fs::copy` whose second argument, is the config: it mentions `spira_toml` or `repo_map` case-insensitively (the env var, or a binding named for it), or, in a file that has `name`, mentions `toml` at all. Arguments are split with balanced brackets, and the call must be in code |

**Exempt.** `spira-config/` by directory. This rule's own source
(`spira-lint/src/rules/config_fence.rs`) names what it hunts.

**Allow list.** `spira/config-fence-allow`: exact repo-relative paths.

```rust
pub struct ConfigAllow(BTreeSet<String>);   // entry == Finding.path
```

**Where the bash heuristic was wrong, and what changed.**
- `parse` matched `toml::from_str`/`toml_edit::` anywhere, comments and strings included,
  and missed `toml::de::`, `toml::Value` and `toml::to_string`. It now matches a use of the
  crate in code.
- `write` in Rust matched `fs::write([^)]*toml`. It stopped at the first `)`, so
  `fs::write(path(), …)` was never inspected, and it matched `toml` anywhere in the argument
  list, content included. It now inspects the destination argument, split with balanced
  brackets. The destination must be the config: a write of some other TOML file does not
  count, such as the test-plan catalogue `a.toml` in `test-plan/tests/validate.rs`, whose
  multi-line `fs::write(` the line regex never saw.
- `write` in shell was a line regex. It caught `> "$SPIRA_TOML_TMP"` (a different variable,
  since `SPIRA_(TOML|REPO_MAP)` was a prefix) and missed `mv "$tmp" "$SPIRA_TOML"` and
  `tee "$SPIRA_TOML"`. It now matches the construct and the exact variable. The write
  checks also no longer run on prose (`.md`, `.fayth`) or on `gate-suites`, which contain
  no shell. A persona that tells an agent to write the config names it, and `name` catches
  that.

## Rule `binary-path-fence`

Ported from `spira/binary-path-fence.sh` (sp-zv7j4).

**Intent.** `conf.sh`'s `spira_bin` is the one binary resolver. A test run's only source of
a binary is `$SPIRA_ARTIFACTS`, and production's only source is the installed release's
`bin/`. A literal build-output path is a second resolver. It silently assumes one tree,
until the tree in front of it is the other.

**Scope.** Every tracked file (untracked files are not scanned, as before).

**Violation.** A line containing `target/release/`, `target/debug/`, `target/aeon/` or
`bin/spira-`, reported as one finding per line: `path:line: <line, leading whitespace
stripped>`. This rule stays textual on purpose. It spans shell, Rust, Makefiles and systemd
units, and a literal path is the same defect in each of them.

**Exempt.**
- Files ending `.md`, `.json` or `.tsv`. Prose and fixture data resolve nothing, and they
  have no comment syntax to carry a marker.
- Binary files (a NUL byte) and files with no non-newline byte.
- The allow file itself, and this rule's own source
  (`spira-lint/src/rules/binary_path_fence.rs`).
- **A line marked `path-ok: <reason>`**, or a line whose previous line carries the marker.
  The marker is the literal `path-ok:` followed by a non-empty reason.

**Allow list.** `spira/binary-path-fence-allow`: exact repo-relative paths. A `#` starts a
comment anywhere on a line, and the entry is trimmed.

```rust
pub struct BinaryPathAllow(BTreeSet<String>);
```

**Where the bash heuristic was wrong.** The marker was the bare substring `path-ok`, so
`# path-ok` with no reason, or any line merely containing the word, exempted itself and
the line after it. The tree carries no marker today, so requiring the reason costs nothing.

## Rule `payload-argv-lint`

Ported from `spira/payload-argv-lint.sh` (sp-o4trx, law-payloads-go-on-stdin).

**Intent.** Some payloads scale with the size of the bead store, such as a `bd` query
result. When one crosses `MAX_ARG_STRLEN` (128 KiB per argv element and per environment
string), the exec fails silently: the caller reads empty output, and empty output reads as
"nothing to do". This happened five times in seventeen days. A payload goes on stdin, or
into a temp file whose *path* travels instead.

**Scope.** Tracked `spira/*.sh` (`*` crosses `/`).

**What counts as a payload.** A shell parameter whose name contains `json`,
case-insensitively. This is the codebase's own convention for a `bd` result. A bounded id
list such as `started_csv` is not a payload.

**Violations.** Judged per simple command recovered by `lex::shell`, including the commands
nested in `$(…)` and backticks. First the command word is found. The rule strips leading
reserved words (`if then else elif do while until ! { time`) and leading `NAME=value`
prefix assignments. It then unwraps `env` (its options, `-u NAME`, and its `NAME=value`
operands become environment assignments), `command`, `exec` and `nohup`. If the command
word is `python3`, `python`, `jq`, `awk`, `gawk` or `mawk`:

| kind | what |
|---|---|
| `env` | an environment assignment for that command whose NAME contains `json`, or whose value expands a json-named parameter |
| `argv` | an argument word that expands a json-named parameter (`$x_json`, `"${x_json:-[]}"`) |

A redirection operand is not an argument (`jq . <<< "$x_json"` is stdin). A word inside a
nested `$(…)` belongs to the nested command, not the outer one. Heredoc bodies are data.
One finding is reported per command per kind, on the line of the first offending word, with
the message `<kind>: $a[, $b] handed to <cmd>`.

**Exempt.** Comments (`lex::shell` drops them), and heredoc bodies.

**Allow list.** `spira/payload-argv-lint-allow` has one regex per line (Rust `regex`
syntax, which accepts the ERE subset used here). Each regex is matched case-insensitively
against `<path>:<line>: <source line, leading whitespace stripped>`, the subject the bash
fence used. A regex that does not compile is `LintError::BadAllow`, never ignored.

```rust
pub struct PayloadAllow(Vec<regex::Regex>);   // any match exempts the finding
```

**Where the bash heuristic was wrong.**
- The ENV shape was the line regex `json_name="plain" … (python3|jq|awk)` on one line:
  - It missed `X_JSON="$(cmd)" python3 …`, a prefix whose value is a command
    substitution, which it excluded to avoid a different false positive. It also missed
    `DATA="$x_json" python3`, where the payload travels under a name without `json`, and
    `env X_JSON=… jq`.
  - It fired on `local a_json="$1"; python3 …` and on `A_JSON="x" cmd | jq .`, where the
    assignment is not in the environment of the command.
- The ARGV shape needed the inline script to open with `python3 -c '`/`jq '`/`awk '`
  immediately, and the payload to be a `"$name"` token after the closing quote. So it
  missed `jq --argjson x "$y_json" '.'`, `awk -v x="$y_json"` and unquoted `$y_json`.
- It scanned heredoc bodies as if they were code.

Each of these is a unit test; the behaviour the bash suite asserted is kept.

## Rule `fence-scripts`

New (sp-tvor6).

**Intent.** Every new fence is a `spira-lint` rule. The bash fences shrink to zero as later
beads port them, and none is added in the meantime.

**Scope.** Tracked and untracked-not-ignored files directly in `spira/` (no deeper `/`)
whose name ends `-fence.sh` or `-lint.sh`. That includes their `test-*` suites, which the
glob matches too.

**Violations.**
- A file in scope that is not on the allow list: `path: a new bash fence/lint script — write
  it as a spira-lint rule instead`.
- An allow entry naming a path that is not in the walk: `<allow-file>:<line>: lists <path>,
  which no longer exists — remove the line (the list only shrinks)`. This makes the list
  exact: porting a fence and deleting its script forces the list to shrink in the same
  change.

**Allow list.** `spira-lint/fence-scripts-allow`, exact paths, generated from the tree at
authorship without the three fences ported here.

```rust
pub struct FenceScriptAllow(Vec<(usize /* line */, String)>);
```

---

# Rules for the lint-shaped suites (sp-l8gl3)

**Intent.** Each suite below was a `test-*.sh` that read files and grepped them: no process,
no database, no clock. As suites they ran inside the gate's container and inside every
round, paid its setup, and were selected only when the diff touched their `# covers:`. As
rules they run on every branch in the gate's lint step, in one walk, in milliseconds, with
no container. Each suite was deleted in the change that added its rule. What needs a
process stays a suite and is named below.

Every rule here reports a missing target file as a finding and an empty scope as a refusal,
never as clean (law-absence-needs-a-positive-control).

## Rule `testlib-migrated`

Ported from `spira/test-testlib-migrated.sh`.

**Intent.** Suites share testlib.sh's assertion primitives. A suite with its own `ok()` or
`want()` counts passes its own way and drifts from the runner's summary.

**Scope.** `spira/test-*.sh` directly in `spira/`.

**Violation.** A line starting `ok()`, `bad()`, `fail()`, `is()`, `want()`, `nowant()`,
`notwant()` or `wantrc()`: one finding per file.

**Allow list.** `spira-lint/testlib-migrated-allow`, exact paths, shrink-only. An entry
naming a suite that no longer redefines a primitive is itself a finding, so the list stays
exact (the suite's "every declared exception still needs one").

## Rule `script-exec`

Ported from `spira/test-script-exec.sh`.

**Intent.** An operator command without the execute bit ships silent.

**Scope.** `spira/*.sh` directly in `spira/`, minus `test-*`.

**Violation.** The file's mode has no execute bit and its first 10 lines do not contain
`Sourced, never executed`. The header is the declaration, so there is no list to drift.
The mode is read from the work tree, as the suite's `[ -x ]` did.

## Rule `event-taxonomy`

Ported from `spira/test-event-taxonomy.sh`.

**Intent.** A typo'd event kind is refused at send time. Declaring the vocabulary and checking
every call site against it at the gate is the difference between a taxonomy and free text.

**Scope.** `spira/*.sh` directly in `spira/` (suites included, as before).

**Violations.**
- `spira_event <kind>` on a non-comment line, where `<kind>` is not declared. The suite's
  `[^#]spira_event` needed a character before the call; a call at the start of a line now
  counts too.
- A declared kind that is not lowercase dotted segments of `[a-z0-9]` or is over 32
  characters (the events table's column).
- A wired call site that no suite can drive and has gone: `spira/gate-check.sh` must contain
  `spira_event ci.failed `; `sentinel/src/check4.rs`, `strand/src/check.rs` and
  `landing-pass/src/push.rs` must contain their kinds as string literals.

**Declared kinds.** `spira-lint/event-kinds`, one per line, allow-file syntax. It moved out
of the suite's `KINDS=` so that adding a kind is an edit to one data file. An empty file, or
no call site found at all, is a refusal.

## Rule `deps-lint`

Ported from `spira/deps-lint.sh` (and its suite `spira/test-deps-lint.sh`); both deleted.

**Intent.** Every external program the harness invokes is declared in `spira/deps.toml`,
which doctor.sh reads. An undeclared program is a dependency nobody installs.

**Scope.** `spira/*.sh` directly in `spira/`, and every `*.rs` in the walk. The script used
`rglob` and skipped `target/`; the walk never holds ignored files, so nothing needs skipping.

**Violations.** In shell, `command -v <prog>` on a non-comment line, unless the match sits
inside an open quote on its line (an odd count of `"` or `'` before it). In Rust, a literal
`Command::new("<prog>")`. `<prog>` matches `[a-z][a-z0-9_-]+`. A program is declared when
`spira/deps.toml` has a `[[dep]]` with that `name`, or it is on the rule's `SYSTEM_ALLOW`
(standard utilities and shell functions, unchanged from the script).

**Refusal.** A missing, unparseable or empty `spira/deps.toml` is `LintError::BadAllow`.

## Rule `covers-entries`

Ported from `spira/test-covers-entries.sh`.

**Intent.** A `# covers:` glob that matches nothing selects its suite for nothing. The suite
can never be chosen for the change it was written to guard.

**Scope.** `spira/test-*.sh` directly in `spira/`.

**The declaration.** Parsed exactly as `suite_covers_of` in `spira/suite-covers.sh` does: the
first `# covers:` line, folded with continuation lines (a comment indented by two or more
blanks that is not a `# word:` directive). `UC-<area>-NN` and `G-NN` tokens are catalogue ids,
not paths, and are skipped.

**Violation.** A path token that resolves to nothing. Resolution is the shell's: `*` and `?`
do not cross `/` or match a leading `.`, and `[…]` is a bracket expression. A token resolves
when it names a file in the walk or a directory holding one. The suite used `[ -e ]`, which
also saw ignored files. A token naming only an ignored file now fails, and that is the
intent: a build output is not something a suite covers.

A suite with no declaration is skipped. If no suite declares anything, that is a refusal.

## Rule `acceptance-run`

Moved from the grep checks of `spira/test-acceptance-run.sh` (the "pattern not found" reds).

**Intent.** `spira/acceptance-run.sh` runs for real only in acceptance, on a clean machine.
Its textual invariants are what the gate can hold it to on every branch.

**Checks.** `acceptance-run.sh` and `acceptance-agent.sh` exist and are executable. Each
fixed string and regex the suite asserted is present or absent, from the rule's `SCRIPT_WANTS`
and `AGENT_WANTS` tables, which name each invariant. The checks that relate lines:
- every `systemctl --user start` has a matching `list-unit-files 'spira-sentinel*.service'`
- no non-comment `sentinel --report`
- no `SPIRA_HOME_REPO` within the scope-label read
- the phase-D override key appended to `$_aged_conf` is in conf.sh's `SPIRA_CONF_KEYS`,
  read statically with `conf-key-registry`'s parser where the suite sourced conf.sh
- every `deploy.sh … "$tag"` passes `--allow-draft`
- every `deploy|uninstall|world|doctor.sh` call and every `. "$1/conf.sh"` read runs under
  `_ci_deploy_env`, with at least 11 wired call sites

**Stays a suite.** `test-acceptance-run.sh` keeps what needs a process: the script's usage
errors, the phase-B output and guard fixtures, and `acceptance-agent.sh` driven twice
against a scratch repository, plus the sweep session with no bead.

## Rule `gate-workflow`

Ported from `spira/test-gate-workflow.sh`, whose header gives the six properties and why each
one cannot be allowed to drift.

**Scope.** `.github/workflows/{gate,release,acceptance,testenv-image}.yml` and the pinned
action fixtures `spira/test-fixtures/ephemeral-ci-v1/{provision,teardown}-action.yml`.

**Checks.** Every assertion of the suite, over the same scopes. Block extraction mirrors the
suite's awk line for line (`block`, `job_if`, `step`, `concurrency`), so a check that read
one job's block still reads only that block. A block that comes back empty is its own
finding, because the checks on it would be vacuous. The suite's section 16a asserted that
`find` counts files in an empty directory, which tests `find` and not the workflow, so it
was dropped, as `docs/test-plan/test-infrastructure.md` row 38 already proposed.

**Tests.** The shipped workflow files are the passing fixture, via `include_str!`. Each
planted violation is a one-line edit of a copy. The test asserts that the edit applied, so
a workflow change that removes the planted text fails the test and does not pass vacuously.

## Rule `conf-key-registry`

Moved from spira-config's `every_conf_sh_key_survives_convert_and_export` (sp-9nljd), which
was red in 13 rounds. It read `spira/conf.sh`, so it was a property of the tree tested as a
property of one crate. A branch that added a key to conf.sh touched no crate and never ran
it at the gate.

**Intent.** Every key conf.sh's `SPIRA_CONF_KEYS` accepts survives spira-config's `convert`
and then `export --sh` unchanged. Otherwise it is dropped on the way to the typed config.

**Check.** Parse the `SPIRA_CONF_KEYS="\n … "\n` block and write each key into a synthetic
spira.conf (the typed keys get typed values, from the rule's `typed_cases`). Then `convert`,
`export_sh`, and undo conf.sh's prefix renaming. One finding per refusal, warning, dropped
key or changed value. The rule links `spira-config` as a library, so the schema it judges
against is the one being built. Fewer than 200 parsed keys is a refusal: that means the
parser is reading the wrong block.

**Stays in spira-config.** The positive control (a planted unknown key is refused by
`convert`) is a property of the crate and stays its unit test.

---

## Tests

Each rule has, in its own module:
- every case of its bash suite, carried over. Where that case encoded a heuristic bug,
  the test asserts the corrected behaviour, and the section above names the change
- a planted violation and its clean counterpart. Each test runs in well under a second,
  on a temp dir.

`lib.rs` runs all rules over one small fixture tree built with `git init`, which covers the
walk itself.
