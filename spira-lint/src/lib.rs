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

use std::cell::OnceCell;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

pub mod lex;
pub mod rules;

/// One file in the walk.
pub struct Entry {
    /// Repo-relative path, `/`-separated, exactly as `git ls-files` printed it.
    pub path: String,
    /// Tracked (in the index) rather than untracked-but-not-ignored.
    pub tracked: bool,
    content: OnceCell<Option<Vec<u8>>>,
}

impl Entry {
    pub fn new(path: impl Into<String>, tracked: bool) -> Entry {
        Entry { path: path.into(), tracked, content: OnceCell::new() }
    }
}

/// The walked tree: a root and the files git knows about under it.
pub struct Tree {
    pub root: PathBuf,
    pub entries: Vec<Entry>,
}

impl Tree {
    /// Walk `root` with git: `ls-files` (tracked) and `ls-files --others --exclude-standard`
    /// (untracked, not ignored). Fails when `root` is not a git work tree — a bad root is
    /// indistinguishable from a clean tree, so it is never read as one.
    pub fn from_git(root: &Path) -> Result<Tree, LintError> {
        let ok = Command::new("git")
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
            let out = Command::new("git")
                .arg("-C")
                .arg(root)
                .args(["ls-files", "-z"])
                .args(extra)
                .output()
                .map_err(|e| LintError::NotARepo(format!("git ls-files: {e}")))?;
            if !out.status.success() {
                return Err(LintError::NotARepo(format!(
                    "git ls-files failed: {}",
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
        Tree { root: root.to_path_buf(), entries }
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

    /// A repo-relative file's text, for allow files. Missing reads as empty — the bash
    /// fences' `2>/dev/null`.
    pub fn read_text(&self, rel: &str) -> String {
        fs::read(self.root.join(rel)).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default()
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
}

impl fmt::Display for LintError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LintError::NotARepo(m) => write!(f, "{m}"),
            LintError::EmptyScope => write!(f, "no files in scope — refusing to report clean"),
            LintError::BadAllow { file, line, reason } => write!(f, "{file}:{line}: {reason}"),
        }
    }
}

/// A fence. See DESIGN.md for each rule's intent, contract and allow-list schema.
pub trait Rule {
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

/// Every rule, in the order they run.
pub fn all_rules() -> Vec<Box<dyn Rule>> {
    vec![
        Box::new(rules::config_fence::ConfigFence),
        Box::new(rules::binary_path_fence::BinaryPathFence),
        Box::new(rules::payload_argv::PayloadArgv),
        Box::new(rules::fence_scripts::FenceScripts),
        Box::new(rules::testlib_migrated::TestlibMigrated),
        Box::new(rules::script_exec::ScriptExec),
        Box::new(rules::event_taxonomy::EventTaxonomy),
        Box::new(rules::deps_lint::DepsLint),
        Box::new(rules::covers_entries::CoversEntries),
        Box::new(rules::acceptance_run::AcceptanceRun),
        Box::new(rules::gate_workflow::GateWorkflow),
        Box::new(rules::conf_key_registry::ConfKeyRegistry),
        Box::new(rules::tmp_leak::TmpLeak),
    ]
}

/// The outcome of one rule over the walk.
pub struct RuleResult {
    pub rule: &'static str,
    pub hint: &'static str,
    pub outcome: Result<Vec<Finding>, LintError>,
}

/// Run `rules` over `tree`.
pub fn run(tree: &Tree, rules: &[Box<dyn Rule>]) -> Vec<RuleResult> {
    rules
        .iter()
        .map(|r| RuleResult {
            rule: r.name(),
            hint: r.hint(),
            outcome: r.check(tree).map(|mut v| {
                v.sort_by(|a, b| (&a.path, a.line).cmp(&(&b.path, b.line)));
                v
            }),
        })
        .collect()
}

#[cfg(test)]
pub(crate) mod testutil {
    use std::fs;
    use std::path::Path;
    use std::process::Command;

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
        pub fn git(&self, args: &[&str]) {
            let st = Command::new("git")
                .arg("-C")
                .arg(&self.0)
                .args(["-c", "user.email=t@example.com", "-c", "user.name=t"])
                .args(args)
                .output()
                .unwrap();
            assert!(st.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&st.stderr));
        }
        pub fn git_init(&self) {
            self.git(&["init", "-q", "-b", "main"]);
        }
    }

}

#[cfg(test)]
mod tests {
    use super::testutil::TempDir;
    use super::*;

    #[test]
    fn pathspec_star_crosses_slashes() {
        assert!(pathspec_match("spira/*.sh", "spira/a.sh"));
        assert!(pathspec_match("spira/*.sh", "spira/hooks/a.sh"));
        assert!(pathspec_match("*.rs", "broker/src/main.rs"));
        assert!(!pathspec_match("spira/*.sh", "spiral/a.sh"));
        assert!(!pathspec_match("spira/gate-suites", "spira/gate-suites2"));
    }

    #[test]
    fn walk_refuses_a_non_repository() {
        let t = TempDir::new("nogit");
        assert!(Tree::from_git(t.path()).is_err());
    }

    #[test]
    fn walk_sees_tracked_and_untracked_but_not_ignored() {
        let t = TempDir::new("walk");
        t.git_init();
        t.write(".gitignore", "ignored.txt\n");
        t.write("a.sh", "x\n");
        t.write("ignored.txt", "x\n");
        t.git(&["add", ".gitignore", "a.sh"]);
        t.write("new.sh", "x\n");
        let tree = Tree::from_git(t.path()).unwrap();
        let got: Vec<(&str, bool)> = tree.entries.iter().map(|e| (e.path.as_str(), e.tracked)).collect();
        assert_eq!(got, vec![(".gitignore", true), ("a.sh", true), ("new.sh", false)]);
    }

    /// The tree-walking rules over one small fixture tree: one planted violation per rule is
    /// found, named by its rule, and nothing else is. The rules that hold named files to a
    /// contract (event-taxonomy, acceptance-run, gate-workflow, conf-key-registry), and tmp-leak, which reads Rust, are
    /// fixtured in their own modules.
    #[test]
    fn all_rules_over_a_fixture_tree() {
        use std::os::unix::fs::PermissionsExt;
        let t = TempDir::new("fixture");
        t.git_init();
        // The planted strings are assembled with concat! so this source file itself carries
        // none of them — config-fence, binary-path-fence and deps-lint all scan *.rs.
        let cfg_name = concat!("spira", ".toml");
        let bin_path = concat!("target", "/release/", "reconciler");
        let prog = concat!("no-such", "-prog");
        t.write("spira/clean.sh", "#!/bin/sh\necho ok\n");
        t.write("spira/cfg.sh", &format!("#!/bin/sh\n# see {cfg_name}\n"));
        t.write("spira/bin.sh", &format!("#!/bin/sh\nBIN=\"$R/{bin_path}\"\n"));
        t.write("spira/payload.sh", "#!/bin/sh\nX_JSON=\"$y\" python3 -c 'print(1)'\n");
        t.write("spira/new-fence.sh", "#!/bin/sh\n");
        t.write("spira/probe.sh", &format!("#!/bin/sh\ncommand -v {prog}\n"));
        t.write("spira/noexec.sh", "#!/bin/sh\n");
        t.write("spira/test-own.sh", "#!/bin/sh\n# covers: spira/clean.sh spira/gone.sh\nok() { :; }\n");
        t.write("spira/deps.toml", "[[dep]]\nname = \"git\"\n");
        t.write("spira/config-fence-allow", "");
        t.write("spira/binary-path-fence-allow", "");
        t.write("spira/payload-argv-lint-allow", "");
        t.write("spira-lint/fence-scripts-allow", "");
        t.write("spira-lint/testlib-migrated-allow", "");
        for f in ["clean", "cfg", "bin", "payload", "new-fence", "probe"] {
            let p = t.path().join(format!("spira/{f}.sh"));
            std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        t.git(&["add", "."]);
        let tree = Tree::from_git(t.path()).unwrap();
        let contract = ["event-taxonomy", "acceptance-run", "gate-workflow", "conf-key-registry", "tmp-leak"];
        let mut rules = all_rules();
        rules.retain(|r| !contract.contains(&r.name()));
        let mut lines = Vec::new();
        for r in &run(&tree, &rules) {
            for f in r.outcome.as_ref().unwrap() {
                lines.push(f.to_string());
            }
        }
        assert_eq!(
            lines,
            vec![
                "config-fence: spira/cfg.sh: name".to_string(),
                format!("binary-path-fence: spira/bin.sh:2: BIN=\"$R/{bin_path}\""),
                "payload-argv-lint: spira/payload.sh:2: env: $X_JSON handed to python3".to_string(),
                "fence-scripts: spira/new-fence.sh: a new bash fence/lint script — write it as a spira-lint rule instead".to_string(),
                "testlib-migrated: spira/test-own.sh: defines its own ok()/bad()/is()/want()/nowant()/wantrc() — source testlib.sh instead".to_string(),
                "script-exec: spira/noexec.sh: not executable — chmod +x it, or declare \"Sourced, never executed\" in its header".to_string(),
                format!("deps-lint: spira/probe.sh:2: {prog}: command -v of a program spira/deps.toml does not declare"),
                "covers-entries: spira/test-own.sh: # covers: token 'spira/gone.sh' matches no file in the tree".to_string(),
            ]
        );
    }
}
