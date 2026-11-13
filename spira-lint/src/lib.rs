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

use std::sync::OnceLock;

const GIT_HANG_GUARD_SECS: &str = "120";
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

pub mod lex;
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
        Entry { path: path.into(), tracked, content: OnceLock::new() }
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
}

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
        Tree { root: root.to_path_buf(), entries, base: None }
    }

    /// The same tree, judged against `base`.
    pub fn with_base(mut self, base: Option<String>) -> Tree {
        self.base = base.filter(|b| !b.trim().is_empty());
        self
    }

    /// `git -C <root> <args>`'s stdout, or its error.
    pub fn git(&self, args: &[&str]) -> Result<Vec<u8>, String> {
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
            Err(format!("git {}: timed out after {}s", args.join(" "), GIT_HANG_GUARD_SECS))
        } else {
            Err(format!("git {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim()))
        }
    }

    /// [`Tree::base`] as a commit id. A rule that compares with the base refuses without one:
    /// comparing against nothing reads as clean.
    pub fn base_commit(&self) -> Result<String, LintError> {
        let Some(base) = self.base.as_deref() else {
            return Err(LintError::Refused(
                "no base to compare against — pass --base <rev> or set SPIRA_GATE_BASE".into(),
            ));
        };
        self.git(&["rev-parse", "--verify", "--quiet", &format!("{base}^{{commit}}")])
            .map(|o| String::from_utf8_lossy(&o).trim().to_string())
            .map_err(|_| {
                LintError::Refused(format!("the base {base} does not resolve to a commit in {}", self.root.display()))
            })
    }

    /// `rel` as it stood at `rev`, or `None` when `rev` has no such file.
    pub fn show(&self, rev: &str, rel: &str) -> Option<String> {
        self.git(&["show", &format!("{rev}:{rel}")]).ok().map(|b| String::from_utf8_lossy(&b).into_owned())
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
        self.entries.binary_search_by(|e| e.path.as_str().cmp(rel)).ok().map(|i| &self.entries[i])
    }

    /// The text of a file in the walk (lossy UTF-8), or `None` when the walk has no such
    /// regular file.
    pub fn text_of(&self, rel: &str) -> Option<String> {
        let e = self.entry(rel)?;
        self.content(e).map(|b| String::from_utf8_lossy(b).into_owned())
    }

    /// The tree's config delta: the base file and every fragment the tree lists, merged.
    pub fn delta_text(&self) -> Option<String> {
        let mut parts: Vec<(String, String)> = self.entries.iter().filter(|e| e.path == "spira/config-delta.toml" || (e.path.starts_with("spira/config-delta.d/") && e.path.ends_with(".toml"))).filter_map(|e| Some((e.path.clone(), self.text_of(&e.path)?))).collect();
        parts.sort();
        spira_config::delta::merge_delta_texts(&parts).ok().flatten()
    }

    /// A repo-relative file's text, for allow files. Missing reads as empty — the bash
    /// fences' `2>/dev/null`.
    pub fn read_text(&self, rel: &str) -> String {
        fs::read(self.root.join(rel)).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default()
    }
}

/// Rules that already judge the branch against the base themselves; `--diff` leaves their
/// findings whole.
pub const BASE_RELATIVE: &[&str] = &["plan-matrix", "lockfile-lint", "config-delta", "tier-budget-allowlist", "tier-budget-area-allowlist", "tier-budget-areas", "workflow-config", "otel-binaries", "journal-reads", "release-tool-exec", "bare-unit-name", "test-real-clock"];

/// What a branch changed relative to `git merge-base <base> HEAD`: per touched file, the
/// new-side line ranges it adds or changes (`None` for a whole file: untracked, or binary).
pub struct DiffScope {
    files: std::collections::BTreeMap<String, Option<Vec<(usize, usize)>>>,
}

impl DiffScope {
    pub fn from_tree(tree: &Tree) -> Result<DiffScope, LintError> {
        let base = tree.base_commit()?;
        let mb = tree
            .git(&["merge-base", &base, "HEAD"])
            .map(|o| String::from_utf8_lossy(&o).trim().to_string())
            .map_err(|e| LintError::Refused(format!("--diff: {e}")))?;
        let out = tree
            .git(&["diff", "-U0", "--no-color", "--no-ext-diff", "--no-renames", &mb])
            .map_err(|e| LintError::Refused(format!("--diff: {e}")))?;
        let mut scope = DiffScope { files: Default::default() };
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
                if p == "/dev/null" {
                    if let Some(o) = old.take() {
                        self.files.entry(o).or_insert_with(|| Some(Vec::new()));
                    }
                }
                if let Some(c) = &cur {
                    self.files.entry(c.clone()).or_insert_with(|| Some(Vec::new()));
                }
            } else if l.starts_with("Binary files ") {
                if let Some(p) = l.strip_suffix(" differ").and_then(|l| l.rsplit(" and b/").next()) {
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
        match (self.files.get(&f.path), f.line) {
            (None, _) => false,
            (Some(None), _) | (Some(Some(_)), None) => true,
            (Some(Some(r)), Some(n)) => r.iter().any(|&(a, b)| a <= n && n <= b),
        }
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
        match self.line {
            Some(n) => write!(f, "{}: {}:{}: {}", self.rule, self.path, n, self.message),
            None => write!(f, "{}: {}: {}", self.rule, self.path, self.message),
        }
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
    BadAllow { file: String, line: usize, reason: String },
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
            let text = tree.content(e).map(|b| String::from_utf8_lossy(b).into_owned())?;
            Some((key.to_string(), text.lines().next().unwrap_or("").trim().to_string()))
        })
        .collect()
}

/// The keys of [`allow_dir`] as `base` had them, read from the single-file list this directory replaced when the base predates it; `None` when there is no base to compare with.
pub fn allow_dir_at(tree: &Tree, dir: &str) -> Option<std::collections::BTreeSet<String>> {
    let base = tree.base_commit().ok()?;
    let pre = format!("{dir}/");
    let out = tree.git(&["ls-tree", "-r", "--name-only", &base, "--", &pre]).ok()?;
    let keys: std::collections::BTreeSet<String> =
        String::from_utf8_lossy(&out).lines().filter_map(|l| l.strip_prefix(&pre).map(str::to_string)).collect();
    if !keys.is_empty() {
        return Some(keys);
    }
    let legacy = dir.strip_suffix(".d")?;
    Some(allow_lines(&tree.show(&base, legacy)?).iter().filter_map(|l| l.split_whitespace().next().map(str::to_string)).collect())
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
    path.strip_prefix(dir)?.strip_prefix('/').filter(|rest| !rest.is_empty() && !rest.contains('/'))
}

/// A line with its leading whitespace stripped (`sed 's/^[[:space:]]*//'`), for messages.
pub fn trim_lead(line: &[u8]) -> String {
    let start = line.iter().position(|b| !b.is_ascii_whitespace()).unwrap_or(line.len());
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
        RuleResult { rule: r.name(), hint: r.hint(), outcome, checked: r.checked(), report: r.report() }
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
    slots.into_iter().map(|r| r.expect("every rule index is claimed by exactly one worker")).collect()
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
    #[test]
    fn diff_scope_keeps_only_changed_lines_and_touched_files() {
        let mut d = DiffScope { files: Default::default() };
        d.parse("diff --git a/x.rs b/x.rs\n--- a/x.rs\n+++ b/x.rs\n@@ -3,0 +4,2 @@\n+a\n+b\n@@ -9 +11 @@\n-c\n+d\n@@ -20,2 +21,0 @@\n-e\n-f\ndiff --git a/y.rs b/y.rs\n--- a/y.rs\n+++ b/y.rs\n@@ -1 +1 @@\n-1\n+2\n");
        let f = |path: &str, line| Finding { rule: "r", path: path.into(), line, message: String::new() };
        assert!(d.keeps(&f("x.rs", Some(4))) && d.keeps(&f("x.rs", Some(5))) && d.keeps(&f("x.rs", Some(11))));
        assert!(!d.keeps(&f("x.rs", Some(3))) && !d.keeps(&f("x.rs", Some(6))) && !d.keeps(&f("x.rs", Some(21))));
        assert!(d.keeps(&f("x.rs", None)) && d.keeps(&f("y.rs", Some(1))));
        assert!(!d.keeps(&f("z.rs", None)) && !d.keeps(&f("z.rs", Some(1))));
    }

    #[test]
    fn pathspec_star_crosses_slashes() {
        assert!(pathspec_match("spira/*.sh", "spira/a.sh"));
        assert!(pathspec_match("spira/*.sh", "spira/hooks/a.sh"));
        assert!(pathspec_match("*.rs", "broker/src/main.rs"));
        assert!(!pathspec_match("spira/*.sh", "spiral/a.sh"));
        assert!(!pathspec_match("spira/gate-suites", "spira/gate-suites2"));
    }
}
