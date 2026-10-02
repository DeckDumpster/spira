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

## Rule `config-literal-fallback`

New (sp-ivfu3); no bash fence precedes it.

**Intent.** Eight release binaries read `env SPIRA_RUN || "/tmp/spira"` instead of
resolving `spira.run` through `spira_config`: right under a systemd unit, which always
sets `SPIRA_RUN` itself, and silently wrong — reading and writing the wrong run
directory — from a bare operator shell. This is the fourth copy of
law-a-binary-resolves-the-config-it-reads in one day (target-reap, inbox-triage, doctor,
now these), so per the harness's own escalation ladder it gets a MECHANISM instead of
another one-off fix.

**Scope.** Every `*.rs` in the walk except `target/`.

**Test code** is the same classifier `tmp-leak`/`script-callers` already share
([`crate::rust_test`]): a whole file under `tests/` or named `tests.rs`, a whole file with
its own inner `#![cfg(test)]`, an item under `#[cfg(test)]` (never `cfg(not(test))`), and
anywhere a test module's own `mod name;` loads transitively.

**Violation**, one finding per site:

- A string literal `"/tmp/spira"` (exact — `"/tmp/spira-run"`/`"/tmp/spira/sub"` do not
  match: a prefix is not the literal this bead's bug used).
- `env::var`/`env::var_os` (optionally `std::`-qualified) called with a `"SPIRA_*"` string
  literal, chained — through any run of `.ok()`/`.map(...)`/`.filter(...)`/`.as_deref()`/
  `.to_owned()`/`.clone()`/`.trim()`/`.to_string()` — to `.unwrap_or(...)` or
  `.unwrap_or_else(...)` whose own argument contains a literal (a `"` anywhere in its text,
  since `unwrap_or_else`'s argument is a closure and the literal default usually sits
  nested inside it — `PathBuf::from("...")`, `"...".to_string()` — not bare; or a bare run
  of ASCII digits). `.unwrap_or_default()` is never a finding: the type's own default (an
  empty string, `PathBuf::new()`) is an absence, never a guessed path.

  The key decides which literal defaults count: [`CONFIG_IDENTITY_KEYS`] (`SPIRA_RUN`,
  `SPIRA_HOME`, `SPIRA_REPO`, `SPIRA_INSTANCE`, `SPIRA_CHAMBER`, `SPIRA_CHAMBER_OVERLAY`,
  `SPIRA_FAYTHS`, `SPIRA_DB`, `SPIRA_WORKSPACES`, `SPIRA_REPO_MAP`, `SPIRA_TOML`,
  `SPIRA_CONF`, `SPIRA_HOME_REPO` — "where/who/which copy") trip it on ANY literal default;
  every other `SPIRA_*` key trips it only when the default is PATH-SHAPED (contains `/`).
  A key outside that list with a short, non-path default (`SPIRA_SYSTEMCTL` defaulting to
  `"systemctl"`, `SPIRA_BD` to `"bd"`, `SPIRA_GH` to `"gh"`) is a PROGRAM NAME this process
  shells out to on `$PATH` at exec time — not a config value `spira_config` owns, and not
  this defect; a `/` in that default would still mean it is secretly a path, so it still
  counts.

**Allow list.** `spira-lint/config-literal-fallback-allow`, exact paths, shrink-only. Not
empty at birth: the rule's own first run over the whole tree found 24 pre-existing sites
outside the eight binaries sp-ivfu3 itself fixed (`broker`, `census`, `cockpit-collect`,
`cockpit/ops`, `doctor`, `gate-run`, `gh-intake`, `groomer`, `install`, `loom`, `queue`,
`reconciler-alert`, `rule`, `sending`, `skew`, `spira-lc`, `spira-world/src/bin/slay.rs`,
`tsd-lifecycle-export`, `watchd`, `work`) — each the same shape, each its own follow-on fix
in the crate it lives in, listed in the allow file rather than bundled into this bead.

**Known limits.** The method-chain scan is lexical, not a real parser: it stops at the
first method name it does not recognise as a passthrough or an `unwrap_or*`, so a chain
that reaches the literal through some OTHER combinator (`.unwrap_or_else(|_| ...).map(...)`
in the other order, or a `match` instead of a chain) is not seen — a false negative, not a
false positive, and the allow list is where any such gap surfaces once a human reads the
code it hid in. A literal that is itself computed (`format!("{x}/spira")`, no literal `/`
of its own but a runtime value that happens to look like a path) is not seen either — this
rule judges what the source SPELLS, not what a given run would print.

## Rule `binary-path-fence` — deleted (sp-gypjk)

Every Spira tool is invoked by its bare name on a PATH the launcher sets (design
runtime-is-a-release, 2026-09-29), so there is no resolver left for a literal build-output
path to be "a second one" of. The design takes no lint for this: the running system is a
release with no checkout beside it, and a stray path fails closed on first use.

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

## Rule `acceptance-run` — retired (sp-ak7qm)

`spira/acceptance-run.sh` moved into `release acceptance` (release/DESIGN.md "acceptance").
Every invariant this rule held the script's text to is now structural in the Rust (one
constructor per kind of call, so a deploy without `--allow-draft` or a tool without the
release's launcher environment cannot be written) or a unit test of the `acceptance`
module (the override key against `SPIRA_CONF_KEYS`, the whole run against a fake host).
`acceptance-agent.sh` stays bash; `test-acceptance-agent.sh` drives it for real.

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

Two properties were added after the port (sp-6cbna): **29**, the suites job is a launcher —
its "Stage the build as a release" step lays out a release from the downloaded build
(`release build --bin-dir`) and writes `SPIRA_RELEASE`, `SPIRA_REPO` (the checkout — a tool
run from the release would otherwise default its repository to the release directory) and a
PATH that starts with that release to `GITHUB_ENV`; and **30**, lints never scan build output — the binaries download
under `runner.temp`, not into the checkout, and the Lints step runs `spira-lint` by name after
asserting it resolves to `$SPIRA_RELEASE/bin/spira-lint`.

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

## Rule `plan-lint`

Ported from `spira/plan-lint.sh`'s default (`--lint`) mode (sp-pppt0). The script itself is
not deleted: its `--orphans` mode is `plan-matrix` above (ported earlier, under sp-ufbkh) and
`--check`/`--gaps` stay bash-callable utilities used by hand and by
`test-plan-matrix-merge.sh` (`--gaps` is a report and was never a failure, so it has no gate
role to port).

**Why a rule.** `plan-lint.sh`'s default mode — every suite declares `# tier:`/`# covers:`,
and every UC id on `# covers:` exists in the typed catalogue — was never wired into any gate
string at all, so a violation reached the tree unchecked. This is the first gate coverage
for it, and it found about 130 pre-existing suites with no `# tier:` and a handful of stale
UC ids the moment it ran over the real corpus.

**Checks**, over `spira/test-*.sh`: missing `# tier:` (`plan_matrix::tier_of`, empty or
absent); missing `# covers:` (`covers_entries::covers_of`, empty or absent); a `UC-*` token
on `# covers:` absent from every `docs/test-plan/*.toml` (loaded fresh via
`test_plan::load_catalogues`; a malformed catalogue is reported as a finding, not a
refusal — the same handling `plan-matrix` gives it). No catalogue directory still checks the
headers; it just cannot check UC ids.

**Allow list.** `spira-lint/plan-lint-allow`: exact suite paths, shrink-only, generated at
authorship from every suite the real tree already violated (the `fence-scripts`/
`testlib-migrated` pattern) — so the rule can gate every *new* suite from day one without
failing every branch on debt it did not add. A listed suite's findings (all of them, not
per-violation) are suppressed; a listed suite with no findings left is itself a finding
("no longer needs an exception").

**Refuses** (exit 3): no suites matched `spira/test-*.sh`. **Positive control:**
`fence: plan-lint checked <n> suites`.

## Rule `testdb-mode-lint`

Ported from `spira/testdb-mode-lint.sh` (sp-pppt0), which is deleted.

**Intent.** Embedded Dolt resets in ~5s; server mode pays a real dolt-beads-test round trip,
median 110s. A suite that pins `SPIRA_TESTDB_MODE=server` without saying why is
indistinguishable, at a glance, from one that needs the real engine.

**Scope.** `spira/test-*.sh`, directly in `spira/`.

**Violation.** A live (non-comment) line containing `SPIRA_TESTDB_MODE=server`
(`export`-prefixed or inline), when the file carries no `# testdb-mode: server — <reason>`
header (non-empty reason after the em dash) anywhere in it. One finding per request line.

**Refuses** (exit 3): no suites in scope. **Positive control:** `fence: testdb-mode-lint
checked <n> suites`.

## Rule `bd-stdin-lint`

Ported from `spira/bd-stdin-lint.sh` (sp-pppt0; defect sp-j5z3), which is deleted.

**Intent.** `bd note <id> - <<EOF` and `bd create ... -d - <<EOF` store the literal `-` and
discard the heredoc body that follows it — six beads shipped with a dash where their body or
notes should be. The stdin forms (`--stdin`, `--body-file -`) are the fix.

**Scope.** Every file under `spira/` or `chamber/` (git's default pathspec `*`, crossing
`/`), one pass per file over both shapes together (never two passes for the same file).

**Violation.** A live (non-comment) line matching either shape:
`bd … note … <space>-<space><<` or `bd … create … (-d<space>-|-d-|--description<space>-)`
followed by `<`, end of line, or a non-`-` character. One finding per line.

**Refuses** (exit 3): no `spira/` or `chamber/` files in scope. **Positive control:**
`fence: bd-stdin-lint checked <n> files`.

## Rule `incident-cause-lint`

Ported from `spira/incident-cause-lint.sh` (sp-pppt0), which is deleted.

**Intent.** A producer that sets `SPIRA_INCIDENT_REF` without `SPIRA_INCIDENT_CAUSE` beside
it files recurrences into the undifferentiated "unrecorded" bucket, collapsing the census
taxonomy a remedy needs to rank failure classes.

**Scope.** `spira/*.sh`, any depth, excluding suites (a basename starting `test-`) — the bash
fence's `grep -rn --include='*.sh' | grep -v '/test-'`.

**Violation.** A live, code (non-comment, not inside a quoted string before the `=`)
`SPIRA_INCIDENT_REF=` assignment with no `SPIRA_INCIDENT_CAUSE` anywhere in its surrounding
14-line window (10 before, 3 after, clamped to the file's bounds).

**Intended difference from the bash fence.** The bash fence computed its window with
`sed -n "$((l-10)),$((l+3))p"`; for a site on line 10 or earlier that produces a negative or
zero start address, which GNU sed rejects as an unrecognised *option* (`-4` looks like a
flag), not a line-address error — so the window search silently sees nothing and the site is
refused even when its declared cause sits two lines above it. This rule clamps the window's
start to line 1 instead, so a site near the top of a short file is judged on what is actually
around it.

**Refuses** (exit 3): no files in scope. **Positive control:** `fence: incident-cause-lint
checked <n> files`.

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
real caller beyond its own tests — `sop write`/`validate`, checking a runbook body before
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

**The box's config file is not an input (sp-g9f3t).** `schema.sh` sources conf.sh, whose
config search reads the operator's own `spira.toml` (explicit `SPIRA_CONF`/`SPIRA_TOML`, else
XDG) — but only when that file is newer than the tree's `spira/chamber/*.fayth`; otherwise it
regenerates a persona-only `spira.toml` into the tree and reads that. On the gate the same
local/main tree and the same binary flipped between 0 and 5 findings on mtimes alone (the
operator's `ask_label = "needs-ryan"` was read or not), and two base trials were judged red
on it. Every `schema.sh` call therefore runs with `SPIRA_CONF`, `SPIRA_TOML` and
`SPIRA_CHAMBER` pinned to paths that do not exist: the names are the tree's own declarations
plus any override the process environment carries (`SPIRA_ASK_LABEL` exported by an aeon
still counts), identical on every box. A label an operator sets only in a config file is not
guarded by this fence; a tree's verdict cannot depend on one box's file.

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

## Rule `gh-intake-lint` — RETIRED (sp-8fsql)

Was: ported from `spira/gh-intake-lint.sh` (deleted, sp-ekkak) to scan `spira/gh-intake.sh`
for a mutating curl flag or a credential reference (law-beads-is-never-public).

Retired rather than kept refusing on a missing target: `spira/gh-intake.sh` itself is
deleted (sp-8fsql; the `gh-intake` crate replaces it). The property this rule policed is now
structural, not scanned — `gh-intake`'s `Http` port (`gh-intake/src/ports.rs`) has no
parameter through which a credential could travel, and its one implementation (`real.rs`)
never reads `GITHUB_TOKEN` or sets an `Authorization:` header. A regex over bash text cannot
regress in a program that has no bash text to carry the regression; see
`gh-intake/DESIGN.md` §2 and §5. Retired rather than ported, per rule one of the rewrite
wave: a check whose subject is gone has no caller left to serve.

## Rule `script-callers`

New (sp-9y0gf; widened to Rust string literals, units and the watchers manifest the same
day, sp-yv4b3). Prototyped as the Concierge scratchpad `sp-missing-scan.py`, which this
rule's suite half is a direct port of.

**Intent.** A rewrite that deletes or renames a script leaves its callers pointed at
nothing, and the gate attributes the resulting red to the base, not the branch that broke
it — `test-batch-red-main.sh` invoked the retired `verdict.sh` for days before anyone
noticed it was never going to pass again. Four callers broke the same way in one day
(`test-batch-red-main.sh` → `verdict.sh`; `queue-watch`/`batcher-cut`/`czar-pass`'s
`SPIRA_FORGE` default → `forge.sh`; `test-mail-deliver.sh`'s `WORLD=world.sh`) because
nothing read a Rust default or a bare suite variable for the name of a script
(law-a-rename-repoints-no-reader).

**Scope**, four kinds of caller:
- `spira/test-*.sh`, directly in `spira/` — suites.
- Every `*.rs` outside `target/`.
- `systemd/*.service`, `systemd/*.timer` — unit templates.
- `spira/watchers`, exactly.

**Suites.** Parsed with `lex::shell`. For every command, `payload_argv::resolve` (shared:
the same `env`/`command`/`exec`/`nohup` unwrap payload-argv-lint already carries) finds the
word actually run, after skipping a leading pass-through placeholder (`"${@}"`, `"$@"`,
`"$*"`, `"${*}"` — `env -i … "${@}" "$WORLD" start`'s shape). That word, and the next one
when it is an interpreter/loader (`bash`, `sh`, `python3`, `python`, `source`, `.`), are the
candidates. A candidate names a script two ways:
1. `$SH`, `${HERE}`, `$SPIRA_HOME` (braced or not) followed by `/` and a `*.sh`/`*.py` path,
   anywhere in the word — resolved as `spira/<path>`.
2. a bare variable the same file assigned a literal, unexpanded, slash-free `*.sh`/`*.py`
   name (`WORLD=world.sh` then `"$WORLD"`) — resolved as `spira/<name>`.

A resolved path missing from the tree is a finding, **unless** the same file creates that
exact basename itself: a redirect (`>`, `>>`, `>|`, `<>`, `&>`, `&>>`) onto it, or it is an
argument to `cp`/`install`/`ln`/`tee`/`write_exe` — `test-world-drain-deadline.sh`'s
`printf … > "$SH/aeon.sh"` before `bash "$SH/aeon.sh"`, and every `test-world-*.sh`'s
`cp "$WORLD_BIN" "$SH/world.sh"`, are exempt this way, deliberately: `SH` there is pointed
at a sandbox, and the suite's whole point is to exercise the real binary under that name.

**Rust string literals.** `lex::rust` classifies the file; every byte run classified
`Literal` (never `Comment`, never plain `Code` — `self.sh()` is not a path) is scanned for
`[A-Za-z0-9_./-]*\.(?:sh|py)`. A slash-free match resolves under `spira/`; a slash-bearing
one is read as the exact path the author wrote. **Test code is out of scope entirely**,
upstream of the scan — not an allow list, a classifier: [`crate::rust_test`] (lifted out of
`tmp-leak`, sp-qgfdi, so the two rules cannot define "is this test code" two different ways)
marks a file test code as a whole by path (`tests/…`, `tests.rs`) or inner
`#![cfg(test)]`, and marks the byte ranges of every `#[cfg(test)]` item (and, transitively,
every file a `mod` declared inside one of those loads). Without this, the rule's own crate
alone produced over a thousand hits — every rule's own fixture tree spells a `*.sh` name as
test data, which is exactly what it should do and exactly what this rule must not read as a
tree reference. A missing path is still exempt when the same file writes it first:
`fs::write`/`File::create`/`File::create_new`/`fs::rename`/`fs::copy` naming the basename
within 200 bytes of the call — a renderer that writes the script before running it.

**Unit templates and the watchers manifest.** Comment lines (`#` or `;` leading) are
blanked first — the watchers manifest's header is almost entirely prose explaining the file
format, and reads full of script names that are not rows. Two forms, over what is left:
`@PLACEHOLDER@/path.sh`, resolved through a known-placeholder table (`SPIRA_HOME`/
`SPIRA_PROD` → `spira/`, `SPIRA_PROD_ROOT` → the repo root, `SPIRA_COCKPIT`/
`SPIRA_PROD_COCK` → `cockpit/`) — an unrecognised placeholder resolves nothing rather than
guess; and a bare `name.sh`/`name.py` token at a word boundary (line start, whitespace, or
`|` — the watchers manifest's field separator), resolved under `spira/`, the one place a
release's own scripts live on the launcher's PATH (sp-gypjk, matching `deps-lint`'s
`SYSTEM_ALLOW` reasoning for the same convention).

**Exempt.** Nothing by path beyond what each scan already excludes above (test code for
the Rust scan; a file's own creation of the target for the suite and Rust scans).

**Allow list.** `spira-lint/script-callers-allow`: exact repo-relative paths, shrink-only —
the `fence-scripts`/`tmp-leak` shape (every finding for a listed path is suppressed, not
one occurrence; a listed path with no findings left is itself a finding, so the list stays
exact). Seeded at authorship with exactly one entry, `spira-world/src/bin/world.rs`: this
rule's own first run against the real tree caught `world stop`/`drain --deadline` still
spawning the retired `spira/slay.sh` (`Command::new("slay.sh")`, sp-6onps repointed
`world.sh`/`aeons.sh`/`ctrl.sh` but missed this one) — genuine, but not a trivial fix here:
a dozen suites stub a fake `$SH/slay.sh` on PATH to intercept the call, so repointing the
binary needs every one of those renamed in the same change. Left listed, named, and
reported rather than folded into this delivery; the allow file's own comment carries the
fix. Every OTHER file this rule's authorship run found (the suite-side `WORLD=world.sh` in
`test-mail-deliver.sh`, sp-yv4b3's three Rust defaults, already fixed before sp-9y0gf) was
trivial and is fixed, not listed.

**Refuses** (exit 3): no file in scope across all four kinds. **Positive control:**
`fence: script-callers checked <n> callers` — one count across every file kind it read,
only on a clean run (the same "only claim 'checked' when it is true" convention
`testdb-mode-lint`/`bd-stdin-lint`/`incident-cause-lint` use).

**Known limits.** The suite scan's `env` unwrap does not special-case every option `env`
takes (only `-u`/`--unset` consumes a following word; any other `-x` is assumed to take
none), and a candidate placed after two nested wrapper commands the unwrap does not know
about is missed, never falsely flagged. The Rust scan's "written nearby" exemption is a
200-byte window after the call, not a parsed argument list — a destination computed far
from its `fs::write` call could still be missed as a false finding; none in the real tree
needed more.

---

## Rule `lib-sh-shims`

Written for the wave-4 close-out audit (sp-hlng2, "wave 4.37: close-out, lib.sh is shims
only"). The family-by-family peel of `wave4-decomposition.md` only stays "lib.sh is shims
only" if nothing quietly grows real logic back into the file every earlier bead emptied.

**Intent.** Every function left in `spira/lib.sh` and `spira/conf.sh` is a one-line shim
onto a binary (the `log`/`die` prelude excepted) or it has a named, reasoned line in this
rule's allow list.

**Check.** Parse `name() {` … lone `}` regions out of each file (this file's own very
consistent style: the header at column 0, the body indented, a lone `}` closing it — a
one-liner like `log() { printf …; }` is also recognised). A function's body is a SHIM when
every statement is set-up — a `local`/plain assignment, env-threading ahead of a call — or
delegation: one call to a known Spira binary, or to another function already defined
earlier in the same file (calling a sibling shim is still a shim, however much work the
sibling itself does). It is a REAL BODY — and needs an allow-list entry — when the body
has a loop (`while`/`for`/`until`), an `if` carrying an `else`/`elif` branch (a bare
`if … ; then … fi` guard with no else is set-up, not logic — the shape
`_counter_events_query` and `lc_release_bead` both use), an inline interpreter script
(`python3 -c`, `perl -e`), or two or more invocations of an external command that is
neither a delegate nor a sibling function (in practice, raw `git` plumbing run more than
once — `content_landed`'s ancestor-then-merge-tree proof is the motivating case).

**Allow list.** `spira-lint/lib-sh-shims-allow`, `<file>:<function>` per line. An entry
naming a function that no longer exists in that file is refused — the list shrinks by
removing the line, not by leaving it to rot. The full audit this rule's own allow list
reflects — every real-bodied function, why, and what blocks porting or retiring it — is in
the sp-hlng2 bead report, not repeated here; most are blocked on groups 5-7 of
`wiki/projects/spira/remaining-bash-inventory.md` (operator surface, install/units, persona
passes — out of wave 4's scope), and `conf.sh`'s own locator/bootstrap family is blocked
structurally: it is the logic that finds and, if needed, converts the config *before* any
`spira-config` subprocess call can be made, so it cannot itself shim onto that subprocess.

**Positive control.** A planted function with two raw `git` calls, shaped exactly like
`content_landed`, is caught by the crate's own unit test — there is no separate "fence:
checked" line; spira-lint's own summary line covers this rule, like most others.

**Known limits.** This is a heuristic over the file's own consistent style, not a bash
parser: a case label's own command, written on the same line as the label, is never
re-examined for a foreign call (no function in either file does that today), and the
command splitter does not track quoting, so a `;`/`&&`/`||`/`|` inside a quoted string on
the same line as another command could misparse. Good enough for this file; not meant to
generalise to shell with a different shape.

---

## Tests

Each rule has, in its own module:
- every case of its bash suite, carried over. Where that case encoded a heuristic bug,
  the test asserts the corrected behaviour, and the section above names the change
- a planted violation and its clean counterpart. Each test runs in well under a second,
  on a temp dir.

`lib.rs` runs all rules over one small fixture tree built with `git init`, which covers the
walk itself.
