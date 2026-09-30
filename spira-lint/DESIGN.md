# spira-lint — design

One binary replaces the harness's bash fences one rule at a time. This document is the
specification each rule is implemented from; the bash scripts were read for their intent
and their test cases, not transcribed. Where a bash fence's heuristic was wrong, this says
so and says what the rule does instead.

## The program

```
spira-lint [--root <dir>] [--only <rule>] [--base <rev>]
```

- `--root` defaults to the git work tree containing the current directory. At the gate that
  is the gate's own worktree (the gate string runs in it), never the repository it came from.
- `--only <rule>` runs one rule; an unknown name is a usage error.
- `--base <rev>` is what the branch is judged against, for the rules that compare
  (`plan-matrix`, `lockfile-lint`, the tier-budget ledgers). It defaults to
  `SPIRA_GATE_BASE`, which the gate always sets. With neither, those rules **refuse** (exit
  3): comparing against nothing reads exactly like a clean comparison.
- **One walk.** `git ls-files -z` (tracked) and `git ls-files -z --others --exclude-standard`
  (untracked, not ignored), once. Each rule filters that walk; no rule lists files itself.
  A file's bytes are read at most once and shared between rules.
- **Output.** One line per finding on stdout, `<rule>: <path>[:<line>]: <message>`, sorted by
  rule order, then path, then line. After a rule's findings, its hint (what to do about it)
  goes to stderr once. A clean rule prints nothing; a clean run prints one summary line to
  stderr, then the positive controls (sp-ufbkh): one `fence: <rule> checked <n> <unit>` per
  rule that reports one (`Rule::checked`: the rules the gate names in
  `gate/src/fence.rs` `LINT_RULE_FENCES`), and last `fence: spira-lint checked <files> files
  (<k> rules: …)`. The gate requires each of those lines on a PASS (gate/DESIGN.md "Every
  fence proves it checked").
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

## Rule `tmp-leak`

New (sp-qgfdi); no bash fence precedes it.

**Intent.** A test's scratch directory under the system temp dir outlives the test unless
something removes it. /tmp is a tmpfs with a fixed inode budget: one `cargo test
--workspace` left 157 entries there, landing-pass alone had left 84,964, and /tmp ran out
of inodes on 2026-09-29. `gate_mode = unit` runs those tests on the host at every gate.
testkit's `TempDir` removes its directory on drop, panics included, so the rule makes it
the only way test code gets scratch space.

**Scope.** Every `*.rs` in the walk except `testkit/` (which implements `TempDir`) and
`target/`.

**Test code** is:
- a whole file under a `tests/` directory (an integration test) or named `tests.rs`;
- a whole file with an inner `#![cfg(test)]`;
- the item under an outer `#[cfg(test)]` (or `cfg(all(test, …))`; never `cfg(not(test))`):
  a `{…}` item is a region of its file, and a `mod name;` makes `name.rs` / `name/mod.rs`
  (or its `#[path]`) a whole test file;
- transitively, the file of any `mod name;` declared inside test code.

**Violation.** A call `temp_dir()` in test code (`std::env::temp_dir()`, `env::temp_dir()`,
or an imported `temp_dir()`), in code and not in a comment or string. One finding per call,
at its line. The rule does not try to decide whether a particular call is later cleaned up:
the ones that were cleaned up by hand were each a copy of `TempDir`, and a second copy is a
second thing to get wrong.

**Allow list.** `spira-lint/tmp-leak-allow`, exact paths, shrink-only; every call in a listed
file is allowed. It starts empty. An entry naming a file with no call is itself a finding.

**Known limits.** A `mod name;` nested inside an inline `mod m { … }` resolves as if it
were declared at the file's top level. A literal `"/tmp/…"` path is not a call and is not
flagged; nothing in the tree creates one. Temp files made by the code under test (a
production `mktemp`) are that code's to clean and outside this rule.

## Rule `plan-matrix`

Ported from `spira/plan-matrix-fence.sh` (sp-ufbkh), which is deleted.

**Why a rule.** The bash fence ran from `gate-touched.sh` only when `SPIRA_GATE_REPO`
resolved to the tree the script sat in. The gate sets `SPIRA_GATE_REPO` to the repository and
runs the gate string in its own worktree, so the two never matched and the fence was skipped
at every gate, exiting 0. As a rule it judges the tree spira-lint walks, which at the gate is
the gate tree, and it cannot skip: it refuses.

**Checks**, against [`--base`]:
1. *Orphans* (`test_plan::orphan_violations`): a use case some `spira/test-*.sh` at the base
   covered on its `# covers:` line and none here covers, with no `[use_case.uncovered]`
   marker in the current catalogue. One finding per use case, path `docs/test-plan`.
2. *The matrix*: `docs/test-plan/*.toml` loads (`test_plan::load_catalogues`; each load error
   is a finding) and the coverage matrix builds and renders, in memory. Nothing is written:
   `plan-matrix.sh` still writes `coverage.json`/`COVERAGE.md` for readers.

Suite headers are read by the one parser the bash used (`covers_of`, and `tier_of` =
`suite_tier_of`: the first `# tier:` before `set -`). The base's suites come from one
`git ls-tree` and one `git cat-file --batch`.

**Refuses** (exit 3): no base; a base that is not a commit; a base with no suites; no
`docs/test-plan/`; a catalogue with no use case. **Positive control:** `fence: plan-matrix
checked <use cases> use-cases (<n> suites here, <m> at the base)`.

## Rule `lockfile-lint`

Ported from `spira/lockfile-lint.sh` (sp-4kws1; sp-ufbkh), which is deleted.

**Intent.** A Cargo.lock bumped by an earlier, unpinned `cargo` can resolve a registry package
to a version the pinned toolchain cannot parse, with no Cargo.toml edit to review. For every
package whose locked version rises between the base and this tree (every one of the base's
versions strictly lower, compared on the first three integer parts; a pre-release is not
judged, a new package is not a bump), the text of `git diff <base> -- '*Cargo.toml'` must
name it as a word. One finding per bump, path `Cargo.lock`.

**Refuses** where the bash skipped with exit 0: no base, no Cargo.lock here, none at the base,
a lock that does not parse or locks nothing. **Positive control:** `fence: lockfile-lint
checked <n> packages`.

## Rules `tier-budget-allowlist`, `tier-budget-area-allowlist`

Ported from `tier-budget.sh lint-allowlist [--area]` (sp-5m133; sp-ufbkh). The bash
subcommands remain for use by hand; the gate runs these.

**Intent.** `spira/tier-budget-allowlist` and `spira/tier-budget-area-allowlist` only shrink.
Each is tab-separated, key in column 1, value in the last column, `#` comments. Against the
base's copy: a key the base lacks is `new entry <k>`; a value higher than the base's
(numerically when both are numbers) is `<k> raised <was> -> <now>`.

**Refuses** where the bash passed ("its introducing commit"): no base, no prior copy at the
base, no ledger here. That pass also passed an unresolvable base having compared nothing.
**Positive control:** `fence: <rule> checked 1 ledger (<n> entries, <m> at the base)` — one
ledger, whatever its size, because a ledger shrunk to nothing is the goal, not a silence.

## Rule `tier-budget-areas`

Ported from `tier-budget.sh check-areas` (sp-ufbkh).

**Intent.** At most one T3 suite per use-case area. A suite's areas are the `<area>` of each
`UC-<area>-NN` token on its `# covers:` line; a suite naming its area twice counts once. An
area with more than one T3 suite fails unless `spira/tier-budget-area-allowlist` records at
least that many. One finding per area, path `area <name>`, naming the suites.

**Scope.** `spira/test-*.sh` directly in `spira/`; an empty scope refuses (the bash returned 0
on no T3 suites, and on no suites at all). **Positive control:** `fence: tier-budget-areas
checked <n> suites`.

---

# Rules ported from the fences wave-brief (sp-ekkak): inventory, literal, scratch, wiki-add,
# tmux-scope, gh-intake

These six ran as standalone bash scripts in the gate string (each with its own `[ -r … ]`
missing-script check and its own `fence: <name> checked …` line). As rules they run inside the
one `spira-lint` invocation the gate already makes; the aggregate `fence: spira-lint checked …`
line covers them, the way it already covers config-fence, binary-path-fence, payload-argv-lint
and fence-scripts. None needed its own entry in `gate/src/fence.rs`'s `LINT_RULE_FENCES` for
the same reason those four don't.

## Rule `inventory`

Ported from `spira/inventory.sh` (deleted).

**Intent.** This repository is meant to be cloned. A comment naming a repository, a deploy
path, a host or a person teaches the next agent to reason about infrastructure that does not
exist, and sometimes to act on it. The check is structural, so it needs no list of the
operator's own names: an absolute path rooted in a home or workspace directory
(`/home/<user>/…`, `/Users/<user>/…`, an absolute path rooted at `/workspaces`), a provenance mark naming a person and
a date (`(per <Name>, YYYY-MM-DD`), and an e-mail address that is neither a reserved example
domain (RFC 2606 / RFC 6761) nor the `git@host` SSH remote form nor a systemd template
instance (`unit@instance.service`, which has the shape of an address but no mail domain ends
this way).

**Scope.** The whole walk (tracked and untracked-not-ignored) minus this rule's own source
(`spira-lint/src/rules/inventory.rs`, which spells out every pattern) and `spira/
inventory-deny`. The exemption is applied inside the check, not in `applies_to`, so an empty
*repository* refuses while an all-exempt one does not read as one — matching the bash
original's plain `${#tracked[@]} -gt 0` check.

**Violation.** One finding per offending file, message a sorted, deduplicated, comma-joined
list of the offending tokens — `config-fence`'s "one finding per file whose message lists the
kinds" shape, not a line-numbered one: the bash original never reported a line either.

**The operator's own additions.** `spira/inventory-deny`, one extended regex per line
(`#`-comments and blanks dropped), joined into the same alternation as the four structural
patterns. It ships empty; a shipped deny-list of somebody else's names would be exactly the
inventory this fence exists to keep out of a shared repository.

**`--scan <file>`.** The one standalone mode spira-lint's CLI supports outside `--only`/`--root`
(`main.rs`): scans one file's content against the same patterns and deny list, printing one
offending token per line, exit 0 either way. `spira/inventory.sh --scan /dev/stdin` had one
real caller beyond its own tests — `sop.sh write`/`validate`, checking a runbook body before
it is staged — and `test-cockpit-remote.sh`'s positive control on two shipped files. Both now
call `"$SPIRA_LINT_BIN" --only inventory --scan <file>`.

**Where the bash heuristic was wrong, and what changed.** A malformed `inventory-deny` entry
made `grep -E` error to stderr while the pipeline still read as "no hits" — an undetected
false-negative that silently disabled every pattern check, structural patterns included. The
rule now refuses (`LintError::BadAllow`) rather than compile a broken alternation and report
clean.

## Rule `literal-lint`

Ported from `spira/literal-lint.sh` (deleted).

**Intent.** `spira/schema.sh` is the one file that declares configured label, status and type
names. A name that appears as a literal in any other source file can disagree with the
declaration when an operator changes the default: `lib.sh:117` grepped `"needs-ryan"` while
`lib.sh:221` read `${SPIRA_ASK_LABEL:-needs-operator}`, and the code default was
`needs-operator`, so on a default install the destructive-procedure fence never matched and
every legitimate halting bead was refused.

**Which names.** `spira/schema.sh names`, then `name <key>` and `default <key>` for every
declared key, filtered to compound tokens (`[a-z0-9-]+` with at least one hyphen — a
single-word name like `plan` or `spike` is excluded, because it appears legitimately as
English prose and a fence with a high false-positive rate is a fence everybody learns to
ignore). **Both values are kept**, union of `name` and `default`: a literal is wrong whether it
happens to match the configured value or the shipped default, and which of the two
`schema_name` returns depends on whether the process environment carries an operator override
— not something to depend on at the gate.

**Reading schema.sh.** The rule shells out to `spira/schema.sh` (a subprocess, the same way
`Tree::from_git` shells out to `git`) rather than re-declaring the vocabulary: the declaration
has one home, and a second copy is exactly the class of drift this fence exists to catch. When
`spira/schema.sh` is missing, not executable, or its `names` call fails or yields nothing, the
rule warns on stderr and falls back to the shipped defaults (`needs-operator`, `needs-ryan`,
`awaiting-ci`, `maechen-sweep`, `maechen-remedy`, `review-finding`, `world-stop`) — never an
empty list, which would make every tree "clean" (law-absence-needs-a-positive-control).

**Scope.** Every tracked file (untracked files are not scanned — the bash original read `git
ls-files`, the index, because what the next commit ships is what matters).

**Exempt.** `schema.sh` and `conf.sh` by basename (the declaration and the operator-facing
default assignments); this rule's own source
(`spira-lint/src/rules/literal_lint.rs`); every `*.md` (prose cannot drift from a declaration
the way a literal comparison can); every `test-*.sh` by basename (fixture data legitimately
carries specific label values); every `*.json` (no comment syntax to carry a `literal-ok`
annotation); a binary file (a NUL byte, or nothing but newlines — PR 331 failed on
`bin/queue-watch` carrying a default label as a string constant, which has no line to
annotate).

**Violation.** A non-comment line (stripped of leading whitespace, not starting `#` or `//` —
both comment syntaxes in this tree) matching a configured name, unless the line or the line
directly above it carries `literal-ok`. One finding per hit, at its line.

**Where the bash heuristic was wrong.** The bash `CONFIGURED_NAMES` array was a hardcoded copy
of `schema.sh`'s defaults, itself already stale by the time the fence was written — it guarded
`needs-operator` on a host running `SPIRA_ASK_LABEL=needs-ryan`, so the exact line that
motivated the lint sailed through reporting "clean". Reading the declaration at check time
(rather than a copy of it, however recently taken) is the fix this rule carries forward
(law-schema-over-code).

## Rule `scratch-fence`

Ported from `spira/scratch-fence.sh` (deleted).

**Intent.** Aeon working notes committed to the harness root ship to every consumer and widen
the suite-selection fallback (a file no suite declares runs the full corpus) on every branch
that follows them — ten accumulated before this fence existed.

**Scope.** Tracked, root-level (no `/` in the path) files matching `sp-*` (an aeon working note
named after a bead) or `*.fixed` (a hand-patched artefact). Untracked files are not scanned —
the bash original read `git ls-files` with no `--others`.

**The refusal is deliberately not `crate::scope`.** An empty *offender* set is the normal,
clean state here, not something to refuse on (unlike every rule whose whole *scope* is small
and violation-shaped, such as `fence-scripts`). What refuses is an empty index — nothing
tracked at all, checked directly against `tree.entries`, matching the bash original's
`git ls-files | wc -l`.

**Override.** `SCRATCH_FENCE_OK=1` in the process environment accepts the tree despite
offenders present — for the one commit that removes them, named in the commit message. This is
the one rule in the crate that reads an environment variable rather than an allow file; the
override is rare, operator-invoked, and already how the bash original worked, so the ported
rule keeps it rather than inventing an allow-list shape nothing else needs.

**A real, non-gate caller.** The tracked `spira/hooks/pre-commit` runs this check directly
(armed by `exclude.sh install`, which points `core.hooksPath` at it) — the local pre-commit
chokepoint, not only the landing gate. It now calls `"$SPIRA_LINT_BIN" --only scratch-fence`
and fails closed (refuses the commit) when `SPIRA_LINT_BIN` is not built, the same way the gate
already fails closed on a missing `spira-lint` binary.

## Rule `wiki-add-fence`

Ported from `spira/wiki-add-fence.sh` (deleted).

**Intent.** Blanket staging on the wiki checkout (`git add -A`, `git add .`, `git commit -a`)
sweeps another actor's uncommitted work into the commit, manufacturing false attribution
(incident: sp-4fl2e). `wiki-commit.sh` is the canonical path; it stages each file explicitly.

**Scope.** Tracked `*.sh` files anywhere in the tree (the bash original scanned `git ls-files --
'*.sh'`, not scoped to `spira/`). Exempt: this rule's own source
(`spira-lint/src/rules/wiki_add_fence.rs`).

**Violation.** A non-comment line mentioning `SPIRA_WIKI` that also matches a blanket-staging
form (`add -A`, `add .` followed by whitespace/`;`/`|`/`&` or end of line, or `commit -a`). One
finding per line, path and line number, message the raw line text (unmodified, matching the
bash original, which did not trim it).

## Rule `tmux-scope-fence`

Ported from `spira/tmux-scope-fence.sh` (deleted).

**Intent.** A suite that calls a tmux session-affecting command, or runs `concierge.sh
start/wake/here/stop`, with no socket of its own drives whatever server the caller's
environment already points at — the operator's own, on the host, twice (sp-pfca0: the cockpit
went down both times).

**Scope.** `spira/test-*.sh`, directly in `spira/` (tracked and untracked-not-ignored — the
bash original globbed the filesystem directly, `"$ROOT"/spira/test-*.sh`, rather than reading
git). Exempt: this rule's own source (`spira-lint/src/rules/tmux_scope_fence.rs`).

**Violation.** A non-comment line invoking one of the 22 tmux session commands
(`new-session`, `kill-server`, …, `source-file`) with no `-L <socket>` on that same line and no
file-wide `TMUX_TMPDIR` (checked as a plain substring anywhere in the file — the shipped
pattern is one `export TMUX_TMPDIR=…` clearing every bare call after it). Separately, when the
file carries neither `CONCIERGE_SOCKET` nor `CONCIERGE_SESSION` anywhere, a non-comment line
invoking `concierge.sh start/wake/here/stop` (with or without a quote before the subcommand).

## Rule `gh-intake-lint`

Ported from `spira/gh-intake-lint.sh` (deleted).

**Intent (law-beads-is-never-public).** The tracker is public and `gh-intake.sh` only reads
it: a mutating curl flag (`-X POST/PATCH/PUT/DELETE`, `--data`, `-d `) is a write GitHub cannot
take back, and a credential reference (`GITHUB_TOKEN`, `github.token`, an `Authorization`
header) is a token the next caller can misuse.

**Scope.** Exactly one file, `spira/gh-intake.sh`. Missing or unreadable refuses
(`LintError::Refused`) rather than reporting clean — the bash original's `[ -r "$TARGET" ] ||
exit 3`.

**Violation.** A non-comment line matching the write or credential pattern. One finding per
line, at its line, message the line text with leading whitespace stripped.

---

## Tests

Each rule has, in its own module:
- every case of its bash suite, carried over. Where that case encoded a heuristic bug,
  the test asserts the corrected behaviour, and the section above names the change
- a planted violation and its clean counterpart. Each test runs in well under a second,
  on a temp dir.

`lib.rs` runs all rules over one small fixture tree built with `git init`, which covers the
walk itself.
