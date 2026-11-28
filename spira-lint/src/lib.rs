//! spira-lint — the harness's static fences as one program.
//!
//! One walk of the tree (`git ls-files`, tracked plus untracked-but-not-ignored, each entry
//! remembering which it was), then every [`Rule`] runs over that one walk. A rule is a
//! module: its name, which files it applies to, what it finds in one file, and a named,
//! shrink-only allow list read from the same allow file the bash fence it replaces used.
//!
//! Output is one line per finding, `<rule>: <path>[:line]: <message>`; any finding is a
//! non-zero exit. A rule that cannot find anything to check refuses to report clean
//! (law-absence-needs-a-positive-control) — that is an error, not a pass.

#[cfg(test)]
#[macro_use]
extern crate spira_test_id;
use std::sync::OnceLock;

const GIT_HANG_GUARD_SECS: &str = "120";
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

pub mod args;
pub mod lex;
pub mod record;
pub mod rules;
pub mod rust_test;

/// One file in the walk.
pub struct Entry {
    /// Repo-relative path, `/`-separated, exactly as `git ls-files` printed it.
    pub path: String,
    /// Tracked (in the index) rather than untracked-but-not-ignored.
    pub tracked: bool,
    content: OnceLock<Option<Vec<u8>>>,
}

impl Entry {
    pub fn new(path: impl Into<String>, tracked: bool) -> Entry {
        Entry {
            path: path.into(),
            tracked,
            content: OnceLock::new(),
        }
    }
}

/// The walked tree: a root and the files git knows about under it.
pub struct Tree {
    pub root: PathBuf,
    pub entries: Vec<Entry>,
    /// The revision a branch is judged against (`--base`, else `SPIRA_GATE_BASE`), for the
    /// rules that compare the tree with what it replaces (`plan-matrix`). `None`: not given,
    /// and such a rule refuses rather than compare against nothing.
    pub base: Option<String>,
    deny_list: Option<String>,
    gate_bead: Option<String>,
    reader: Option<GitReader>,
}

/// A stand-in for spawning git, for unit tests.
pub type GitReader = Box<dyn Fn(&[&str]) -> Result<Vec<u8>, String> + Send + Sync>;

impl Tree {
    /// Walk `root` with git: `ls-files` (tracked) and `ls-files --others --exclude-standard`
    /// (untracked, not ignored). Fails when `root` is not a git work tree — a bad root is
    /// indistinguishable from a clean tree, so it is never read as one.
    pub fn from_git(root: &Path) -> Result<Tree, LintError> {
        let ok = spira_config::bounded::bounded("git")
            .arg("-C")
            .arg(root)
            .args(["rev-parse", "--git-dir"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if !ok {
            return Err(LintError::NotARepo(format!(
                "{} is not a git repository — nothing to scan",
                root.display()
            )));
        }
        let list = |extra: &[&str]| -> Result<Vec<String>, LintError> {
            // batch-job: lists the whole tree, which outlasts a 5s call deadline on a loaded box
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(root)
                .args(["ls-files", "-z"])
                .args(extra)
                .output()
                .map_err(|e| LintError::NotARepo(format!("git ls-files: {e}")))?;
            if !out.status.success() {
                return Err(LintError::NotARepo(format!(
                    "git ls-files failed ({}): {}",
                    out.status,
                    String::from_utf8_lossy(&out.stderr).trim()
                )));
            }
            Ok(out
                .stdout
                .split(|b| *b == 0)
                .filter(|s| !s.is_empty())
                .map(|s| String::from_utf8_lossy(s).into_owned())
                .collect())
        };
        let tracked = list(&[])?;
        let others = list(&["--others", "--exclude-standard"])?;
        Ok(Tree::from_paths(root, tracked, others))
    }

    /// A tree from explicit path lists — what `from_git` builds, and what a test builds
    /// without paying for a git repository.
    pub fn from_paths<I, J, S, T>(root: &Path, tracked: I, untracked: J) -> Tree
    where
        I: IntoIterator<Item = S>,
        J: IntoIterator<Item = T>,
        S: Into<String>,
        T: Into<String>,
    {
        let mut entries: Vec<Entry> = tracked.into_iter().map(|p| Entry::new(p, true)).collect();
        entries.extend(untracked.into_iter().map(|p| Entry::new(p, false)));
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        entries.dedup_by(|a, b| a.path == b.path);
        Tree {
            root: root.to_path_buf(),
            entries,
            base: None,
            deny_list: None,
            gate_bead: None,
            reader: None,
        }
    }

    /// The same tree, judged against `base`.
    pub fn with_base(mut self, base: Option<String>) -> Tree {
        self.base = base.filter(|b| !b.trim().is_empty());
        self
    }

    /// The same tree, with the test deny-list file the caller handed (`--deny-list`).
    pub fn with_deny_list(mut self, path: Option<String>) -> Tree {
        self.deny_list = path;
        self
    }

    pub fn deny_list(&self) -> Option<&str> {
        self.deny_list.as_deref()
    }

    /// The same tree, with the item the gate is judging (`--gate-bead`).
    pub fn with_gate_bead(mut self, id: Option<String>) -> Tree {
        self.gate_bead = id;
        self
    }

    pub fn gate_bead(&self) -> Option<&str> {
        self.gate_bead.as_deref()
    }

    /// The same tree, answering git reads from `reader` instead of spawning git.
    pub fn with_git(mut self, reader: GitReader) -> Tree {
        self.reader = Some(reader);
        self
    }

    /// `git -C <root> <args>`'s stdout, or its error.
    pub fn git(&self, args: &[&str]) -> Result<Vec<u8>, String> {
        if let Some(r) = &self.reader {
            return r(args);
        }
        // batch-job: a local repo read that stalls for tens of seconds on a loaded host; 120 s hang guard
        let out = std::process::Command::new("timeout")
            .arg(GIT_HANG_GUARD_SECS)
            .arg("git")
            .arg("-C")
            .arg(&self.root)
            .args(args)
            .output()
            .map_err(|e| format!("git {}: {e}", args.join(" ")))?;
        if out.status.success() {
            Ok(out.stdout)
        } else if out.status.code() == Some(124) {
            Err(format!(
                "git {}: timed out after {}s",
                args.join(" "),
                GIT_HANG_GUARD_SECS
            ))
        } else {
            Err(format!(
                "git {}: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr).trim()
            ))
        }
    }

    /// The commit the branch forked from [`Tree::base`]: `git merge-base <base> HEAD`, not the
    /// base's tip, so what landed on the base after the fork is never the branch's own change.
    /// A rule that compares with the base refuses without one: comparing against nothing reads
    /// as clean.
    pub fn base_commit(&self) -> Result<String, LintError> {
        let Some(base) = self.base.as_deref() else {
            return Err(LintError::Refused(
                "no base to compare against — pass --base <rev> or set SPIRA_GATE_BASE".into(),
            ));
        };
        let tip = self
            .git(&[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("{base}^{{commit}}"),
            ])
            .map(|o| String::from_utf8_lossy(&o).trim().to_string())
            .map_err(|_| {
                LintError::Refused(format!(
                    "the base {base} does not resolve to a commit in {}",
                    self.root.display()
                ))
            })?;
        self.git(&["merge-base", &tip, "HEAD"])
            .map(|o| String::from_utf8_lossy(&o).trim().to_string())
            .ok()
            .filter(|f| !f.is_empty())
            .ok_or_else(|| LintError::Refused(format!("{base} shares no history with HEAD")))
    }

    /// `rel` as it stood at `rev`, or `None` when `rev` has no such file.
    pub fn show(&self, rev: &str, rel: &str) -> Option<String> {
        self.git(&["show", &format!("{rev}:{rel}")])
            .ok()
            .map(|b| String::from_utf8_lossy(&b).into_owned())
    }

    /// The file's bytes, read once and cached; `None` when it is not a regular file (a
    /// tracked path deleted from the work tree, a directory, a dangling link) — the bash
    /// fences' `[ -f "$f" ] || continue`.
    pub fn content<'a>(&self, e: &'a Entry) -> Option<&'a [u8]> {
        e.content
            .get_or_init(|| {
                let p = self.root.join(&e.path);
                match fs::metadata(&p) {
                    Ok(m) if m.is_file() => fs::read(&p).ok(),
                    _ => None,
                }
            })
            .as_deref()
    }

    /// The walk's entry for `rel`, if the walk has one.
    pub fn entry(&self, rel: &str) -> Option<&Entry> {
        self.entries
            .binary_search_by(|e| e.path.as_str().cmp(rel))
            .ok()
            .map(|i| &self.entries[i])
    }

    /// The text of a file in the walk (lossy UTF-8), or `None` when the walk has no such
    /// regular file.
    pub fn text_of(&self, rel: &str) -> Option<String> {
        let e = self.entry(rel)?;
        self.content(e)
            .map(|b| String::from_utf8_lossy(b).into_owned())
    }

    /// The tree's config delta: the base file and every fragment the tree lists, merged.
    pub fn delta_text(&self) -> Result<Option<String>, String> {
        let mut parts: Vec<(String, String)> = self
            .entries
            .iter()
            .filter(|e| {
                e.path == "spira/config-delta.toml"
                    || (e.path.starts_with("spira/config-delta.d/") && e.path.ends_with(".toml"))
            })
            .filter_map(|e| Some((e.path.clone(), self.text_of(&e.path)?)))
            .collect();
        parts.sort();
        spira_config::delta::merge_delta_texts(&parts)
    }

    /// A repo-relative file's text, for allow files. Missing reads as empty — the bash
    /// fences' `2>/dev/null`.
    pub fn read_text(&self, rel: &str) -> String {
        fs::read(self.root.join(rel))
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .unwrap_or_default()
    }
}

/// Rules that already judge the branch against the base themselves; `--diff` leaves their
/// findings whole.
pub const BASE_RELATIVE: &[&str] = &[
    "plan-matrix",
    "lockfile-lint",
    "config-delta",
    "config-delta-folded",
    "workflow-config",
    "otel-binaries",
    "bin-tests-skipped",
    "journal-reads",
    "release-tool-exec",
    "bare-unit-name",
    "test-real-clock",
    "registry-readers",
    "no-new-shell-suite",
];

/// What a branch changed relative to `git merge-base <base> HEAD`: per touched file, the
/// new-side line ranges it adds or changes (`None` for a whole file: untracked, or binary).
pub struct DiffScope {
    files: std::collections::BTreeMap<String, Option<Vec<(usize, usize)>>>,
}

impl DiffScope {
    pub fn from_tree(tree: &Tree) -> Result<DiffScope, LintError> {
        let base = tree.base_commit()?;
        let out = tree
            .git(&[
                "diff",
                "-U0",
                "--no-color",
                "--no-ext-diff",
                "--no-renames",
                &base,
            ])
            .map_err(|e| LintError::Refused(format!("--diff: {e}")))?;
        let mut scope = DiffScope {
            files: Default::default(),
        };
        scope.parse(&String::from_utf8_lossy(&out));
        for e in tree.entries.iter().filter(|e| !e.tracked) {
            scope.files.insert(e.path.clone(), None);
        }
        Ok(scope)
    }

    fn parse(&mut self, diff: &str) {
        let mut cur: Option<String> = None;
        let mut old: Option<String> = None;
        for l in diff.lines() {
            if let Some(p) = l.strip_prefix("--- ") {
                old = p.strip_prefix("a/").map(str::to_string);
            } else if let Some(p) = l.strip_prefix("+++ ") {
                cur = p.strip_prefix("b/").map(str::to_string);
                if p == system_paths::DEV_NULL {
                    if let Some(o) = old.take() {
                        self.files.entry(o).or_insert_with(|| Some(Vec::new()));
                    }
                }
                if let Some(c) = &cur {
                    self.files
                        .entry(c.clone())
                        .or_insert_with(|| Some(Vec::new()));
                }
            } else if l.starts_with("Binary files ") {
                if let Some(p) = l
                    .strip_suffix(" differ")
                    .and_then(|l| l.rsplit(" and b/").next())
                {
                    self.files.insert(p.to_string(), None);
                }
            } else if let (Some(h), Some(c)) = (l.strip_prefix("@@ "), cur.as_ref()) {
                let plus = h.split(' ').find(|t| t.starts_with('+')).unwrap_or("+0");
                let mut it = plus[1..].split(',');
                let start: usize = it.next().and_then(|n| n.parse().ok()).unwrap_or(0);
                let len: usize = it.next().and_then(|n| n.parse().ok()).unwrap_or(1);
                if len > 0 {
                    if let Some(Some(r)) = self.files.get_mut(c) {
                        r.push((start, start + len - 1));
                    }
                }
            }
        }
    }

    /// Whether a finding is on code this branch adds or changes. A finding with no line is
    /// whole-file: kept when the diff touches the file at all.
    pub fn keeps(&self, f: &Finding) -> bool {
        let at_location = match (self.files.get(&f.path), f.line) {
            (None, _) => false,
            (Some(None), _) | (Some(Some(_)), None) => true,
            (Some(Some(r)), Some(n)) => r.iter().any(|&(a, b)| a <= n && n <= b),
        };
        at_location || self.message_names_touched_file(&f.message)
    }

    fn message_names_touched_file(&self, message: &str) -> bool {
        message
            .split(|c: char| c.is_whitespace() || "\"'()[]<>,;=`{}|".contains(c))
            .map(|t| t.trim_end_matches(['.', ':']))
            .map(|t| match t.rsplit_once(':') {
                Some((head, tail))
                    if !tail.is_empty() && tail.bytes().all(|b| b.is_ascii_digit()) =>
                {
                    head
                }
                _ => t,
            })
            .any(|t| self.files.contains_key(t))
    }
}

/// One thing a rule found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub rule: &'static str,
    pub path: String,
    pub line: Option<usize>,
    pub message: String,
}

impl fmt::Display for Finding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        render_finding(f, self.rule, &self.path, self.line, &self.message)
    }
}

/// The one text form of a finding, `<rule>: <path>[:line]: <message>`.
pub fn render_finding(
    f: &mut fmt::Formatter<'_>,
    rule: &str,
    path: &str,
    line: Option<usize>,
    message: &str,
) -> fmt::Result {
    match line {
        Some(n) => write!(f, "{rule}: {path}:{n}: {message}"),
        None => write!(f, "{rule}: {path}: {message}"),
    }
}

/// Why a rule could not produce a verdict. Every variant is a refusal (exit 3), never a
/// pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LintError {
    /// The walk could not run.
    NotARepo(String),
    /// Nothing in scope: indistinguishable from a matcher that never fires.
    EmptyScope,
    /// An allow-file line that cannot mean anything.
    BadAllow {
        file: String,
        line: usize,
        reason: String,
    },
    /// The rule cannot check: what it needs is missing (a base, an input). Named, never a pass.
    Refused(String),
}

impl fmt::Display for LintError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LintError::NotARepo(m) => write!(f, "{m}"),
            LintError::EmptyScope => write!(f, "no files in scope — refusing to report clean"),
            LintError::BadAllow { file, line, reason } => write!(f, "{file}:{line}: {reason}"),
            LintError::Refused(m) => write!(f, "{m}"),
        }
    }
}

/// A rule's positive-control slot: set during [`Rule::check`], read after. A `Cell` that is
/// `Sync`, so [`run`] can check rules on worker threads (each rule is checked by exactly one
/// thread; the lock is never contended).
#[derive(Debug, Default)]
pub struct SyncCell<T: Copy>(std::sync::Mutex<T>);

impl<T: Copy> SyncCell<T> {
    pub fn new(v: T) -> Self {
        SyncCell(std::sync::Mutex::new(v))
    }
    pub fn get(&self) -> T {
        *self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
    pub fn set(&self, v: T) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = v;
    }
}

/// A fence. See DESIGN.md for each rule's intent, contract and allow-list schema. `Send +
/// Sync`: [`run`] checks independent rules concurrently over the one shared walk.
pub trait Rule: Send + Sync {
    /// The rule's name: `--only <name>`, and the first field of every finding.
    fn name(&self) -> &'static str;

    /// Repo-relative path of this rule's shrink-only allow list.
    fn allow_file(&self) -> Option<&'static str> {
        None
    }

    /// Whether `e` is in this rule's scope.
    fn applies_to(&self, e: &Entry) -> bool;

    /// Run the rule over the walk.
    fn check(&self, tree: &Tree) -> Result<Vec<Finding>, LintError>;

    /// What a reader of a finding should do about it; printed once, to stderr.
    fn hint(&self) -> &'static str {
        ""
    }

    /// The rule's own positive control after a clean check — how much it checked, and of
    /// what — for a rule the gate expects to see by name (sp-ufbkh). `None` for the rest:
    /// spira-lint's own line covers them.
    fn checked(&self) -> Option<(usize, String)> {
        None
    }

    /// Text a report-mode rule prints to stdout after its check, in place of findings: a rule
    /// that measures a migration rather than fences it exits 0 and still has to be read.
    fn report(&self) -> Option<String> {
        None
    }
}

/// Where the registered config keys live: one file per key.
pub(crate) const CONF_D: &str = "spira/conf.d/";

/// The keys registered in `spira/conf.d/`, from the walk.
pub(crate) fn registered_keys(tree: &Tree) -> std::collections::BTreeSet<String> {
    tree.entries
        .iter()
        .filter_map(|e| e.path.strip_prefix(CONF_D))
        .filter(|k| !k.contains('/') && !k.is_empty())
        .map(str::to_string)
        .collect()
}

/// The files `rule` applies to, refusing an empty scope.
pub fn scope<'t>(tree: &'t Tree, rule: &dyn Rule) -> Result<Vec<&'t Entry>, LintError> {
    let s: Vec<&Entry> = tree.entries.iter().filter(|e| rule.applies_to(e)).collect();
    if s.is_empty() {
        Err(LintError::EmptyScope)
    } else {
        Ok(s)
    }
}

/// A shrink-only allow list kept as a directory, one file per entry: the path under `dir` is
/// the key and the file's first line is the value. Two branches removing different entries
/// touch different files and merge.
pub fn allow_dir(tree: &Tree, dir: &str) -> Vec<(String, String)> {
    let pre = format!("{dir}/");
    tree.entries
        .iter()
        .filter_map(|e| {
            let key = e.path.strip_prefix(&pre)?;
            let text = tree
                .content(e)
                .map(|b| String::from_utf8_lossy(b).into_owned())?;
            Some((
                key.to_string(),
                text.lines().next().unwrap_or("").trim().to_string(),
            ))
        })
        .collect()
}

/// The keys of [`allow_dir`] as `base` had them, read from the single-file list this directory replaced when the base predates it; `None` when there is no base to compare with.
pub fn allow_dir_at(tree: &Tree, dir: &str) -> Option<std::collections::BTreeSet<String>> {
    let base = tree.base_commit().ok()?;
    let pre = format!("{dir}/");
    let out = tree
        .git(&["ls-tree", "-r", "--name-only", &base, "--", &pre])
        .ok()?;
    let keys: std::collections::BTreeSet<String> = String::from_utf8_lossy(&out)
        .lines()
        .filter_map(|l| l.strip_prefix(&pre).map(str::to_string))
        .collect();
    if !keys.is_empty() {
        return Some(keys);
    }
    let legacy = dir.strip_suffix(".d")?;
    Some(
        allow_lines(&tree.show(&base, legacy)?)
            .iter()
            .filter_map(|l| l.split_whitespace().next().map(str::to_string))
            .collect(),
    )
}

/// Non-blank, non-`#` lines, verbatim.
pub fn allow_lines(text: &str) -> Vec<String> {
    text.lines()
        .filter(|l| {
            let t = l.trim_start();
            !(t.is_empty() || t.starts_with('#'))
        })
        .map(str::to_string)
        .collect()
}

/// Git's default pathspec wildcard: `*` matches any run of characters, `/` included, and
/// `?` any one character (fnmatch without FNM_PATHNAME).
pub fn pathspec_match(pat: &str, path: &str) -> bool {
    fn go(p: &[u8], s: &[u8]) -> bool {
        match p.split_first() {
            None => s.is_empty(),
            Some((b'*', rest)) => (0..=s.len()).any(|i| go(rest, &s[i..])),
            Some((b'?', rest)) => !s.is_empty() && go(rest, &s[1..]),
            Some((c, rest)) => s.first() == Some(c) && go(rest, &s[1..]),
        }
    }
    go(pat.as_bytes(), path.as_bytes())
}

/// The basename of `path` when it sits directly in `dir` (no deeper `/`) — a shell's
/// `"$dir"/*` glob.
pub fn direct_child<'a>(path: &'a str, dir: &str) -> Option<&'a str> {
    path.strip_prefix(dir)?
        .strip_prefix('/')
        .filter(|rest| !rest.is_empty() && !rest.contains('/'))
}

/// `spira/test-*.sh`, directly in spira/: what the suite runner would run as a suite.
pub(crate) fn is_suite(path: &str) -> bool {
    direct_child(path, "spira").is_some_and(|b| b.starts_with("test-") && b.ends_with(".sh"))
}

/// A line with its leading whitespace stripped (`sed 's/^[[:space:]]*//'`), for messages.
pub fn trim_lead(line: &[u8]) -> String {
    let start = line
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .unwrap_or(line.len());
    String::from_utf8_lossy(&line[start..]).into_owned()
}

/// Split into awk's records: on `\n`, a trailing newline not starting an empty record.
pub fn lines(content: &[u8]) -> Vec<&[u8]> {
    let body = content.strip_suffix(b"\n").unwrap_or(content);
    if content.is_empty() {
        return Vec::new();
    }
    body.split(|b| *b == b'\n').collect()
}

/// Every rule, in rule-file name order: each `rules/*.rs` exports `rules()` and build.rs collects them.
pub fn all_rules() -> Vec<Box<dyn Rule>> {
    all_rules_with_source()
        .into_iter()
        .map(|(_, r)| r)
        .collect()
}

/// Every rule with the repo-relative path of the module that defines it, a fact of the build.
pub fn all_rules_with_source() -> Vec<(&'static str, Box<dyn Rule>)> {
    include!(concat!(env!("OUT_DIR"), "/all_rules.rs"))
}

/// The outcome of one rule over the walk.
pub struct RuleResult {
    pub rule: &'static str,
    pub hint: &'static str,
    pub outcome: Result<Vec<Finding>, LintError>,
    /// [`Rule::checked`], read after the check.
    pub checked: Option<(usize, String)>,
    /// [`Rule::report`], read after the check.
    pub report: Option<String>,
}

/// Run `rules` over `tree`, results in `rules`' order. The rules are independent reads of
/// one walk, and each is a CPU-bound scan of the tree, so they are checked on up to
/// [`MAX_WORKERS`] threads: run serially, the gate's spira-lint step was the bulk of every
/// gate's fences phase. Output order (and so every finding and positive-control line) is
/// unchanged: it is decided here, by index, never by which thread finished first.
pub fn run(tree: &Tree, rules: &[Box<dyn Rule>]) -> Vec<RuleResult> {
    let one = |r: &dyn Rule| {
        let outcome = r.check(tree).map(|mut v| {
            v.sort_by(|a, b| (&a.path, a.line).cmp(&(&b.path, b.line)));
            v
        });
        RuleResult {
            rule: r.name(),
            hint: r.hint(),
            outcome,
            checked: r.checked(),
            report: r.report(),
        }
    };
    let workers = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .min(MAX_WORKERS)
        .min(rules.len());
    if workers <= 1 {
        return rules.iter().map(|r| one(r.as_ref())).collect();
    }
    let next = std::sync::atomic::AtomicUsize::new(0);
    let mut slots: Vec<Option<RuleResult>> = (0..rules.len()).map(|_| None).collect();
    std::thread::scope(|s| {
        let handles: Vec<_> = (0..workers)
            .map(|_| {
                s.spawn(|| {
                    let mut done = Vec::new();
                    loop {
                        let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let Some(r) = rules.get(i) else { break };
                        done.push((i, one(r.as_ref())));
                    }
                    done
                })
            })
            .collect();
        for h in handles {
            for (i, r) in h.join().unwrap_or_else(|e| std::panic::resume_unwind(e)) {
                slots[i] = Some(r);
            }
        }
    });
    slots
        .into_iter()
        .map(|r| r.expect("every rule index is claimed by exactly one worker"))
        .collect()
}

fn counted(list: &[(&str, usize)]) -> String {
    list.iter()
        .map(|(n, c)| format!("{n} ({c})"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The one stderr line that ends a red run: which rules refused, which errored, and which
/// only printed report-mode lines, so a report's hits are never read as the cause.
pub fn red_summary(
    refused: &[(&str, usize)],
    errored: &[&str],
    reported: &[(&str, usize)],
) -> String {
    let mut s = String::from("spira-lint: RED —");
    let mut clauses = Vec::new();
    if !refused.is_empty() {
        clauses.push(format!("refused by {}", counted(refused)));
    }
    clauses.push(format!(
        "errored: {}",
        if errored.is_empty() {
            "none".into()
        } else {
            errored.join(", ")
        }
    ));
    if !reported.is_empty() {
        clauses.push(format!(
            "reported only, not refusing: {}",
            counted(reported)
        ));
    }
    s.push(' ');
    s.push_str(&clauses.join("; "));
    s
}

/// Whether `--diff` scope `scope` keeps `f` of `rule`; no scope keeps all.
pub fn kept_by(scope: Option<&DiffScope>, rule: &str, f: &Finding) -> bool {
    scope.is_none_or(|s| BASE_RELATIVE.contains(&rule) || s.keeps(f))
}

/// [`red_summary`] over `results`, counting findings kept by `scope` (as the printed ones are).
/// `None` when no rule refused or errored.
pub fn red_summary_of(results: &[RuleResult], scope: Option<&DiffScope>) -> Option<String> {
    let (mut refused, mut errored, mut reported) = (Vec::new(), Vec::new(), Vec::new());
    for r in results {
        if let Some(text) = &r.report {
            let n = text.lines().filter(|l| !l.trim().is_empty()).count();
            if n > 0 {
                reported.push((r.rule, n));
            }
        }
        match &r.outcome {
            Ok(v) => {
                let n = v.iter().filter(|f| kept_by(scope, r.rule, f)).count();
                if n > 0 {
                    refused.push((r.rule, n));
                }
            }
            Err(_) => errored.push(r.rule),
        }
    }
    (!refused.is_empty() || !errored.is_empty()).then(|| red_summary(&refused, &errored, &reported))
}

/// The most threads [`run`] checks rules on.
pub const MAX_WORKERS: usize = 8;

#[cfg(test)]
pub(crate) mod testutil {
    use std::fs;
    use std::path::Path;

    /// A scratch directory removed on drop (testkit's, with this crate's fixture helpers).
    pub struct TempDir(pub testkit::TempDir);

    impl TempDir {
        pub fn new(tag: &str) -> TempDir {
            TempDir(testkit::TempDir::new(&format!("spira-lint-{tag}")))
        }
        pub fn path(&self) -> &Path {
            &self.0
        }
        pub fn write(&self, rel: &str, content: &str) {
            let p = self.0.join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, content).unwrap();
        }
        pub fn remove(&self, rel: &str) {
            fs::remove_file(self.0.join(rel)).unwrap();
        }
    }
}

#[cfg(test)]
mod tests {

    use super::*;

    #[test_id(tb07su8)]
    #[test]
    fn diff_scope_diffs_against_the_fork_not_the_base_tip() {
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let log = seen.clone();
        let t = Tree::from_paths(
            Path::new("nowhere"),
            Vec::<String>::new(),
            Vec::<String>::new(),
        )
        .with_base(Some("main".into()))
        .with_git(Box::new(move |a| {
            log.lock().unwrap().push(a.join(" "));
            match a[0] {
                "rev-parse" => Ok(b"B\n".to_vec()),
                "merge-base" => Ok(b"F\n".to_vec()),
                _ => Ok(Vec::new()),
            }
        }));
        DiffScope::from_tree(&t).unwrap();
        let seen = seen.lock().unwrap();
        assert!(
            seen.iter()
                .any(|c| c.starts_with("diff") && c.ends_with(" F")),
            "{seen:?}"
        );
    }

    #[test_id(tzydbae)]
    #[test]
    fn base_commit_with_no_merge_base_is_refused() {
        let t = Tree::from_paths(
            Path::new("nowhere"),
            Vec::<String>::new(),
            Vec::<String>::new(),
        )
        .with_base(Some("main".into()))
        .with_git(Box::new(|a| match a[0] {
            "rev-parse" => Ok(b"B\n".to_vec()),
            _ => Err("no merge base".into()),
        }));
        match t.base_commit() {
            Err(LintError::Refused(m)) => {
                assert!(m.contains("main shares no history with HEAD"), "{m}")
            }
            other => panic!("{other:?}"),
        }
    }
    #[test_id(tcrhqf9)]
    #[test]
    fn diff_scope_keeps_only_changed_lines_and_touched_files() {
        let mut d = DiffScope {
            files: Default::default(),
        };
        d.parse("diff --git a/x.rs b/x.rs\n--- a/x.rs\n+++ b/x.rs\n@@ -3,0 +4,2 @@\n+a\n+b\n@@ -9 +11 @@\n-c\n+d\n@@ -20,2 +21,0 @@\n-e\n-f\ndiff --git a/y.rs b/y.rs\n--- a/y.rs\n+++ b/y.rs\n@@ -1 +1 @@\n-1\n+2\n");
        let f = |path: &str, line| Finding {
            rule: "r",
            path: path.into(),
            line,
            message: String::new(),
        };
        assert!(
            d.keeps(&f("x.rs", Some(4)))
                && d.keeps(&f("x.rs", Some(5)))
                && d.keeps(&f("x.rs", Some(11)))
        );
        assert!(
            !d.keeps(&f("x.rs", Some(3)))
                && !d.keeps(&f("x.rs", Some(6)))
                && !d.keeps(&f("x.rs", Some(21)))
        );
        assert!(d.keeps(&f("x.rs", None)) && d.keeps(&f("y.rs", Some(1))));
        assert!(!d.keeps(&f("z.rs", None)) && !d.keeps(&f("z.rs", Some(1))));
    }

    #[test_id(tmkyy4x)]
    #[test]
    fn diff_scope_keeps_a_finding_whose_message_names_a_touched_path() {
        let mut d = DiffScope {
            files: Default::default(),
        };
        d.parse("diff --git a/spira/lib-test-install.sh b/spira/lib-test-install.sh\n--- a/spira/lib-test-install.sh\n+++ b/spira/lib-test-install.sh\n@@ -1 +1 @@\n-a\n+b\ndiff --git a/spira/gone.sh b/spira/gone.sh\n--- a/spira/gone.sh\n+++ /dev/null\n@@ -1 +0,0 @@\n-x\n");
        let allow = "spira-lint/config-key-registered-allow";
        let f = |msg: &str| Finding {
            rule: "config-key-registered",
            path: allow.into(),
            line: Some(892),
            message: msg.into(),
        };
        assert!(d.keeps(&f("lists SPIRA_TOML for spira/lib-test-install.sh, which no longer names it unregistered — remove the line (the list only shrinks)")));
        assert!(d.keeps(&f("stale: (spira/lib-test-install.sh:12).")));
        assert!(!d.keeps(&f(
            "lists SPIRA_TOML for spira/other.sh, which no longer names it"
        )));
        assert!(!d.keeps(&f(
            "lists SPIRA_TOML for lib-test-install.sh, which no longer names it"
        )));
        assert!(d.keeps(&f(
            "lists SPIRA_TOML for spira/gone.sh, which no longer names it"
        )));
    }

    #[test_id(tfdpzbv)]
    #[test]
    fn pathspec_star_crosses_slashes() {
        assert!(pathspec_match("spira/*.sh", "spira/a.sh"));
        assert!(pathspec_match("spira/*.sh", "spira/hooks/a.sh"));
        assert!(pathspec_match("*.rs", "broker/src/main.rs"));
        assert!(!pathspec_match("spira/*.sh", "spiral/a.sh"));
        assert!(!pathspec_match("spira/lib.sh", "spira/lib.sh2"));
    }

    #[test_id(tl32hs3)]
    #[test]
    fn every_base_relative_name_is_a_rule_and_no_new_shell_suite_is_one() {
        let names: Vec<&str> = all_rules().iter().map(|r| r.name()).collect();
        for n in BASE_RELATIVE {
            assert!(names.contains(n), "{n} is not a rule");
        }
        assert!(BASE_RELATIVE.contains(&"no-new-shell-suite"));
    }

    #[test_id(tqtqaiw)]
    #[test]
    fn red_summary_names_refused_and_reported_separately() {
        let l = red_summary(
            &[("inventory", 53), ("literal-lint", 8)],
            &[],
            &[("ffx-vocabulary", 479)],
        );
        assert_eq!(
            l,
            "spira-lint: RED — refused by inventory (53), literal-lint (8); errored: none; reported only, not refusing: ffx-vocabulary (479)"
        );
    }

    #[test_id(tisedfx)]
    #[test]
    fn red_summary_refused_only_has_no_reported_clause() {
        let l = red_summary(&[("inventory", 1)], &[], &[]);
        assert_eq!(
            l,
            "spira-lint: RED — refused by inventory (1); errored: none"
        );
    }

    #[test_id(t8xahvt)]
    #[test]
    fn red_summary_errored_only_names_the_rule() {
        let l = red_summary(&[], &["deps-lint"], &[]);
        assert_eq!(l, "spira-lint: RED — errored: deps-lint");
    }

    struct Stub(&'static str, bool, Option<&'static str>);
    impl Rule for Stub {
        fn name(&self) -> &'static str {
            self.0
        }
        fn applies_to(&self, _: &Entry) -> bool {
            true
        }
        fn check(&self, _: &Tree) -> Result<Vec<Finding>, LintError> {
            Ok(if self.1 {
                vec![Finding {
                    rule: self.0,
                    path: "a".into(),
                    line: Some(1),
                    message: "m".into(),
                }]
            } else {
                Vec::new()
            })
        }
        fn report(&self) -> Option<String> {
            self.2.map(str::to_string)
        }
    }

    #[test_id(tjs8x4c)]
    #[test]
    fn a_run_over_an_enforcing_and_a_report_rule_names_each_in_its_own_clause() {
        let t = Tree::from_paths(Path::new("nowhere"), ["a"], Vec::<String>::new());
        let rules: Vec<Box<dyn Rule>> = vec![
            Box::new(Stub("enforcer", true, None)),
            Box::new(Stub(
                "reporter",
                false,
                Some("reporter: a:1\nreporter: a:2\n"),
            )),
        ];
        let l = red_summary_of(&run(&t, &rules), None).unwrap();
        assert_eq!(l, "spira-lint: RED — refused by enforcer (1); errored: none; reported only, not refusing: reporter (2)");
        let rules: Vec<Box<dyn Rule>> =
            vec![Box::new(Stub("reporter", false, Some("reporter: a:1\n")))];
        assert_eq!(red_summary_of(&run(&t, &rules), None), None);
    }
}
