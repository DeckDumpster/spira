//! A registered repository other than the harness: its tiers run in its own image.
//!
//! The repository declares its entry in the tree under test (`.spira/testenv.toml`), the
//! way the gate string lives in `gate.steps`: the entry changes in the same commit as the
//! code it tests.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::Deserialize;

use crate::runtime::{ContainerRuntime, RunRequest, RC_CANCELLED, RC_DEADLINE};

pub const DECL: &str = ".spira/testenv.toml";

/// podman's own failure (no such image, cannot start): the harness, not the tier.
const RC_PODMAN: i32 = 125;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Decl {
    pub image: String,
    #[serde(default = "default_workdir")]
    pub workdir: String,
    #[serde(default)]
    pub timeout: Option<u64>,
    #[serde(default)]
    pub default: Vec<String>,
    /// The fast compile-and-lint tiers a tip must pass before it may be submitted.
    #[serde(default)]
    pub lint: Vec<String>,
    pub tier: BTreeMap<String, Tier>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tier {
    pub argv: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub timeout: Option<u64>,
    /// Overrides the repository's `image` for this tier only (a Node check beside Rust tiers).
    #[serde(default)]
    pub image: Option<String>,
}

fn default_workdir() -> String {
    "/work".into()
}

fn plausible_tier(n: &str) -> bool {
    !n.is_empty() && n.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

impl Decl {
    pub fn parse(text: &str) -> Result<Decl, String> {
        let d: Decl = spira_config::parse_foreign(text).map_err(|e| format!("{DECL}: {e}"))?;
        if d.image.trim().is_empty() {
            return Err(format!("{DECL}: image is empty"));
        }
        if d.tier.is_empty() {
            return Err(format!("{DECL}: declares no tier"));
        }
        for (n, t) in &d.tier {
            if !plausible_tier(n) {
                return Err(format!("{DECL}: tier name {n:?} is not a plain word"));
            }
            if t.argv.is_empty() {
                return Err(format!("{DECL}: tier {n} has no argv"));
            }
            if t.image.as_deref().is_some_and(|i| i.trim().is_empty()) {
                return Err(format!("{DECL}: tier {n}: image is empty"));
            }
        }
        if let Some(n) = d.default.iter().find(|n| !d.tier.contains_key(*n)) {
            return Err(format!("{DECL}: default names undeclared tier {n}"));
        }
        if let Some(n) = d.lint.iter().find(|n| !d.tier.contains_key(*n)) {
            return Err(format!("{DECL}: lint names undeclared tier {n}"));
        }
        Ok(d)
    }

    /// The image tier `name` runs in: its own when it names one, else the repository's.
    pub fn image_for(&self, name: &str) -> &str {
        self.tier.get(name).and_then(|t| t.image.as_deref()).unwrap_or(&self.image)
    }

    pub fn known(&self, name: &str) -> bool {
        self.tier.contains_key(name)
    }

    /// With no `--suites`: the declared default, else every tier.
    pub fn default_tiers(&self) -> Vec<String> {
        if self.default.is_empty() {
            self.tier.keys().cloned().collect()
        } else {
            self.default.clone()
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Pass,
    Red,
    Fault(&'static str),
}

const CACHE_TARGET: &str = "/var/cache/spira-foreign/target";
const CACHE_CARGO: &str = "/var/cache/spira-foreign/cargo";
const LOCK_POLL: Duration = Duration::from_secs(2);

/// A repository's warm build state on the host, kept between runs: cargo's target dir and
/// registry, and the lock that lets one tier of that repository run at a time.
pub struct Cache {
    dir: PathBuf,
}

impl Cache {
    pub fn new(root: &Path, repo: &str) -> Cache {
        let name: String = repo
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') { c } else { '_' })
            .collect();
        Cache { dir: root.join(name) }
    }

    fn prepare(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(self.dir.join("target"))?;
        std::fs::create_dir_all(self.dir.join("cargo"))
    }

    /// The volumes and variables a tier's cargo runs on; a variable the tier declares wins.
    fn mounts(&self) -> Vec<(PathBuf, String)> {
        vec![
            (self.dir.join("target"), CACHE_TARGET.into()),
            (self.dir.join("cargo"), CACHE_CARGO.into()),
        ]
    }

    fn env(&self) -> [(&'static str, &'static str); 2] {
        [("CARGO_TARGET_DIR", CACHE_TARGET), ("CARGO_HOME", CACHE_CARGO)]
    }

    /// Wait for this repository's lock; Err names the fault when the deadline passes or the
    /// run is cancelled first.
    fn lock(&self, deadline: Option<Instant>) -> Result<std::fs::File, &'static str> {
        loop {
            if let Some(f) = crate::worktree::try_lock(&self.dir.join("lock")) {
                return Ok(f);
            }
            if crate::runtime::cancelled() {
                return Err("cancelled");
            }
            if deadline.is_some_and(|d| Instant::now() >= d) {
                return Err("deadline-suites");
            }
            std::thread::sleep(LOCK_POLL);
        }
    }
}

pub struct TierRun<'a> {
    pub cache: &'a Cache,
    pub rt: &'a dyn ContainerRuntime,
    pub decl: &'a Decl,
    pub tree: &'a Path,
    pub results: &'a Path,
    pub key: &'a str,
    pub deadline: Option<Instant>,
}

impl TierRun<'_> {
    pub fn run(&self, name: &str) -> (Outcome, u64) {
        let tier = &self.decl.tier[name];
        let t0 = Instant::now();
        if self.cache.prepare().is_err() {
            return (Outcome::Fault("cache-dir"), 0);
        }
        let _held = match self.cache.lock(self.deadline) {
            Ok(f) => f,
            Err(reason) => return (Outcome::Fault(reason), t0.elapsed().as_secs()),
        };
        let cname = format!("testenv-foreign-{}-{name}", &self.key[..self.key.len().min(12)]);
        let mut env: Vec<(String, String)> = tier.env.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        for (k, v) in self.cache.env() {
            if !tier.env.contains_key(k) {
                env.push((k.into(), v.into()));
            }
        }
        let req = RunRequest {
            name: cname.clone(),
            image: self.decl.image_for(name).to_string(),
            mount: self.tree.to_path_buf(),
            workdir: self.decl.workdir.clone(),
            env,
            mounts: self.cache.mounts(),
            argv: tier.argv.clone(),
            timeout: tier.timeout.or(self.decl.timeout).map(Duration::from_secs),
            deadline: self.deadline,
            output: self.results.join(format!("{name}.log")),
        };
        let rc = self.rt.run_once(&req).rc;
        self.rt.purge(&cname);
        let outcome = match rc {
            0 => Outcome::Pass,
            RC_DEADLINE => Outcome::Fault("deadline-suites"),
            RC_CANCELLED => Outcome::Fault("cancelled"),
            RC_PODMAN => Outcome::Fault("container-run"),
            _ => Outcome::Red,
        };
        (outcome, t0.elapsed().as_secs())
    }
}

/// The first thing a failed tier's log says to fix: a compiler diagnostic with its
/// `file:line`, else the first error line, else the log's last line.
pub fn first_error(log: &str) -> Option<String> {
    let lines: Vec<&str> = log.lines().collect();
    let at = |from: usize| {
        lines[from..].iter().find_map(|l| l.trim_start().strip_prefix("--> ").map(|loc| loc.trim().to_string()))
    };
    if let Some(i) = lines.iter().position(|l| {
        let t = l.trim_start();
        t.starts_with("error") && !t.starts_with("error: could not compile") && !t.starts_with("error: aborting")
    }) {
        let msg = lines[i].trim();
        return Some(match at(i) {
            Some(loc) => format!("{loc}: {msg}"),
            None => msg.to_string(),
        });
    }
    if let Some(i) = lines.iter().position(|l| l.trim_start().starts_with("warning") && !l.contains("generated")) {
        if let Some(loc) = at(i) {
            return Some(format!("{loc}: {}", lines[i].trim()));
        }
    }
    lines.iter().rev().find(|l| !l.trim().is_empty()).map(|l| l.trim().to_string())
}

pub fn tier_line(name: &str, o: Outcome, secs: u64) -> String {
    match o {
        Outcome::Pass => format!("  {name:<32} ok      {secs}s"),
        Outcome::Red => format!("  {name:<32} RED     after {secs}s"),
        Outcome::Fault(r) => format!("  {name:<32} FAULT   {r} after {secs}s"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OK: &str = "image = \"localhost/x:1\"\ndefault = [\"unit\"]\n[tier.unit]\nargv = [\"bash\", \"deploy/ci.sh\", \"unit\"]\n[tier.ui]\nargv = [\"bash\", \"deploy/ci.sh\", \"ui\"]\ntimeout = 5\n";

    #[test]
    fn a_declaration_parses_and_names_its_default() {
        let d = Decl::parse(OK).unwrap();
        assert_eq!(d.default_tiers(), vec!["unit"]);
        assert!(d.known("ui") && !d.known("nosuch"));
        assert_eq!(d.workdir, "/work");
    }

    #[test]
    fn lint_names_only_declared_tiers() {
        let d = Decl::parse("image = \"i\"\nlint = [\"a\"]\n[tier.a]\nargv=[\"x\"]\n").unwrap();
        assert_eq!(d.lint, vec!["a"]);
        let e = Decl::parse("image = \"i\"\nlint = [\"z\"]\n[tier.a]\nargv=[\"x\"]\n").unwrap_err();
        assert!(e.contains("undeclared tier z"), "{e}");
    }

    #[test]
    fn the_first_error_names_file_and_line() {
        let log = "   Compiling x\nerror: this argument is a mutable reference\n  --> src/binder.rs:222:9\n   |\nerror: could not compile `x`\n";
        assert_eq!(first_error(log).unwrap(), "src/binder.rs:222:9: error: this argument is a mutable reference");
        assert_eq!(first_error("ok\nboom\n").unwrap(), "boom");
        assert_eq!(first_error(""), None);
    }

    #[test]
    fn a_tier_image_overrides_the_repo_image_for_that_tier_only() {
        let d = Decl::parse("image = \"r\"\n[tier.a]\nargv=[\"x\"]\n[tier.b]\nargv=[\"x\"]\nimage = \"n\"\n").unwrap();
        assert_eq!(d.image_for("a"), "r");
        assert_eq!(d.image_for("b"), "n");
        let e = Decl::parse("image = \"r\"\n[tier.a]\nargv=[\"x\"]\nimage = \" \"\n").unwrap_err();
        assert!(e.contains("tier a: image is empty"), "{e}");
    }

    #[test]
    fn no_default_means_every_tier() {
        let d = Decl::parse("image = \"i\"\n[tier.b]\nargv=[\"x\"]\n[tier.a]\nargv=[\"x\"]\n").unwrap();
        assert_eq!(d.default_tiers(), vec!["a", "b"]);
    }

    #[test]
    fn bad_declarations_are_refused_by_name() {
        for (text, needle) in [
            ("image = \"i\"\n", "tier"),
            ("image = \"\"\n[tier.a]\nargv=[\"x\"]\n", "image"),
            ("image = \"i\"\n[tier.a]\nargv=[]\n", "no argv"),
            ("image = \"i\"\ndefault=[\"z\"]\n[tier.a]\nargv=[\"x\"]\n", "undeclared tier z"),
            ("image = \"i\"\nstray = 1\n[tier.a]\nargv=[\"x\"]\n", "stray"),
            ("image = \"i\"\n[tier.\"a/b\"]\nargv=[\"x\"]\n", "plain word"),
        ] {
            let e = Decl::parse(text).unwrap_err();
            assert!(e.contains(needle), "{text:?} -> {e}");
        }
    }
}
