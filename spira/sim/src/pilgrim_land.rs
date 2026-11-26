//! A pilgrim's change through a round: the fixture tree a pilgrim world carries so a round has
//! something to build, and `land`, which cuts a one-member round from the pilgrim's branch the
//! way the cut does: claims from the member's diff, rows lifted, the tests run, a green landing
//! activating the claimed rows and clearing the bead's own.

use crate::world::{self, git, REPO_NAME};
use batcher_cut::io::{self, Land, Repo};
use batcher_cut::vm::DenySnapshot;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

const TESTS_DEADLINE: Duration = Duration::from_secs(900); // batch-job: a cold offline build and run of the fixture crates
const TREE: &[(&str, &str)] = &[
    ("Cargo.toml", include_str!("../fixtures/pilgrim-tree/Cargo.toml.txt")),
    ("pilgrim-fixture/test-id/Cargo.toml", include_str!("../fixtures/pilgrim-tree/test-id.Cargo.toml.txt")),
    ("pilgrim-fixture/test-id/src/lib.rs", include_str!("../fixtures/pilgrim-tree/test-id.lib.rs.txt")),
    ("pilgrim-fixture/crate/Cargo.toml", include_str!("../fixtures/pilgrim-tree/crate.Cargo.toml.txt")),
    ("pilgrim-fixture/crate/src/lib.rs", include_str!("../fixtures/pilgrim-tree/crate.lib.rs.txt")),
];
/// The stamped test the fixture tree carries, for a pilgrim to disable and fix.
pub const FIXTURE_TEST: &str = "test:tpgfix1";

/// Replaces the world's tree with one that builds offline (a workspace of two small crates) and
/// lands it on the world's trunk and landing branch.
pub fn install_tree(work: &Path, base: &str) -> Result<(), String> {
    write_tree(work)?;
    git(work, &["add", "-f", "Cargo.toml", "pilgrim-fixture"])?;
    git(work, &["commit", "-q", "-m", "sim pilgrim fixture tree"])?;
    git(work, &["push", "-q", "origin", "main"])?;
    git(work, &["branch", "-f", base, "main"])?;
    Ok(())
}

pub fn write_tree(work: &Path) -> Result<(), String> {
    for (path, text) in TREE {
        let at = work.join(path);
        if let Some(dir) = at.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        std::fs::write(&at, text).map_err(|e| format!("{}: {e}", at.display()))?;
    }
    Ok(())
}

/// What a round asks of the registry; the world's `test-registry` answers it in a pilgrim world.
pub trait RoundRegistry {
    fn deny_list(&self) -> Result<String, String>;
    fn activate(&self, test: &str, reason: &str) -> Result<(), String>;
    fn clear_landed(&self, bead: &str) -> Result<Vec<String>, String>;
}

pub struct TestRun {
    pub green: bool,
    /// Names of the test fns that ran and passed.
    pub passed: Vec<String>,
    pub output: String,
}

pub trait RunTests {
    fn run(&self, tree: &Path, skip_fns: &[String]) -> Result<TestRun, String>;
}

#[derive(Debug)]
pub struct Outcome {
    pub bead: String,
    pub round: String,
    pub green: bool,
    pub lifted: Vec<(String, Option<String>)>,
    pub skipped: Vec<String>,
    pub ran: Vec<String>,
    pub landed_at: Option<String>,
    pub activated: Vec<String>,
    pub not_activated: Vec<(String, String)>,
    pub cleared: Vec<String>,
    pub output: String,
}

impl Outcome {
    pub fn report(&self) -> String {
        let mut out = format!("round {}: {}\n", self.round, if self.green { "green" } else { "red" });
        for (test, by) in &self.lifted {
            match by {
                Some(member) => out.push_str(&format!("lifted {test} (body changed by {member})\n")),
                None => out.push_str(&format!("lifted {test} (owned by {})\n", self.bead)),
            }
        }
        for t in &self.ran {
            out.push_str(&format!("ran {t}\n"));
        }
        for t in &self.skipped {
            out.push_str(&format!("skipped {t}\n"));
        }
        match &self.landed_at {
            Some(sha) => out.push_str(&format!("landed {} at {sha}\n", self.bead)),
            None => out.push_str(&format!("not landed: {}\n{}\n", self.bead, self.output.trim_end())),
        }
        for t in &self.activated {
            out.push_str(&format!("activated {t}\n"));
        }
        for (t, why) in &self.not_activated {
            out.push_str(&format!("not activated {t}: {why}\n"));
        }
        for t in &self.cleared {
            out.push_str(&format!("cleared {t}\n"));
        }
        out
    }
}

fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        if e.file_type().is_ok_and(|t| t.is_dir()) {
            if e.file_name() != "target" && e.file_name() != ".git" {
                rs_files(&p, out);
            }
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

/// (`test:<id>`, fn name) of every stamped test under `tree`.
pub fn stamped_tests(tree: &Path) -> Vec<(String, String)> {
    let mut files = vec![];
    rs_files(tree, &mut files);
    files
        .iter()
        .filter_map(|f| std::fs::read(f).ok())
        .flat_map(|src| spira_lint::rust_test::test_sites(&src))
        .filter_map(|s| Some((format!("test:{}", s.id?), s.fn_name)))
        .collect()
}

/// Lands branch `spira/<bead>` of `work` onto `base` as a round of one member, or reports why
/// the round was red. A failed registry write after a green landing is reported, never undone.
pub fn land(work: &Path, base: &str, bead: &str, reg: &dyn RoundRegistry, tests: &dyn RunTests) -> Result<Outcome, String> {
    if bead.is_empty() || !bead.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return Err(format!("{bead:?} is not a bead id"));
    }
    let branch = format!("spira/{bead}");
    let rev = |r: &str| git(work, &["rev-parse", "--verify", &format!("{r}^{{commit}}")]).map(|s| s.trim().to_string());
    let base_sha = rev(base).map_err(|e| format!("landing ref {base}: {e}"))?;
    let tip = rev(&branch).map_err(|e| format!("commit your change to branch {branch} in {}: {e}", work.display()))?;
    if base_sha == tip {
        return Err(format!("{branch} holds nothing beyond {base}"));
    }
    git(work, &["merge-base", "--is-ancestor", &base_sha, &tip]).map_err(|_| format!("{branch} does not sit on {base}: rebase it"))?;

    let round = format!("pilgrim-{bead}");
    let repo = Repo { name: REPO_NAME.to_string(), path: work.to_path_buf(), base: base.to_string(), forge: work.to_path_buf(), land: Land::Local };
    let deny = DenySnapshot { text: reg.deny_list()?, at: 0, lifted: vec![], claimed: vec![] };
    let claims: Vec<(String, String)> = io::member_claims(&repo, &base_sha, &tip, &deny.test_rows()).into_iter().map(|t| (t, bead.to_string())).collect();
    let (running, lifted) = deny.lifting(&[bead.to_string()], &claims);

    let tree = TreeAt::extract(work, &tip)?;
    let names = stamped_tests(&tree.0);
    let fn_of = |id: &str| names.iter().find(|(i, _)| i == id).map(|(_, f)| f.clone());
    let skipped: Vec<String> = running.test_rows();
    let skip_fns: Vec<String> = skipped.iter().filter_map(|t| fn_of(t)).collect();
    let run = tests.run(&tree.0, &skip_fns)?;
    let ran: Vec<String> = lifted.iter().map(|r| r.test.clone()).filter(|t| fn_of(t).is_some_and(|f| run.passed.contains(&f))).collect();
    let mut out = Outcome {
        bead: bead.to_string(),
        round: round.clone(),
        green: run.green,
        lifted: lifted.iter().map(|r| (r.test.clone(), r.claimed_by.clone())).collect(),
        skipped,
        ran,
        landed_at: None,
        activated: vec![],
        not_activated: vec![],
        cleared: vec![],
        output: run.output,
    };
    if !run.green {
        return Ok(out);
    }
    git(work, &["branch", "-f", base, &tip])?;
    out.landed_at = Some(tip);
    let standing: Vec<(String, String)> = lifted.iter().filter_map(|r| Some((r.test.clone(), r.claimed_by.clone()?))).collect();
    out.not_activated = io::activate_claimed_with(&mut |t, why| reg.activate(t, why), &round, &standing);
    out.activated = standing.into_iter().map(|(t, _)| t).filter(|t| !out.not_activated.iter().any(|(f, _)| f == t)).collect();
    match reg.clear_landed(bead) {
        Ok(cleared) => out.cleared = cleared,
        Err(e) => out.not_activated.push((format!("rows naming {bead}"), e)),
    }
    Ok(out)
}

struct TreeAt(PathBuf);

impl TreeAt {
    fn extract(work: &Path, rev: &str) -> Result<TreeAt, String> {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("pilgrim-round-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let tree = TreeAt(dir);
        let tar = tree.0.join(".tree.tar");
        world::run(Command::new("git").arg("-C").arg(work).args(["archive", "--format=tar", "-o"]).arg(&tar).arg(rev), TESTS_DEADLINE)?;
        world::run(Command::new("tar").arg("-xf").arg(&tar).arg("-C").arg(&tree.0), TESTS_DEADLINE)?;
        let _ = std::fs::remove_file(&tar);
        Ok(tree)
    }
}

impl Drop for TreeAt {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The round's unit tier on the fixture tree: `cargo test` offline, less the skipped tests.
pub struct CargoTests;

impl RunTests for CargoTests {
    fn run(&self, tree: &Path, skip_fns: &[String]) -> Result<TestRun, String> {
        let mut cmd = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()));
        cmd.current_dir(tree).args(["test", "--offline", "--workspace", "--target-dir"]).arg(tree.join("target")).arg("--");
        // Empty, not removed: removed leaves a cargo config's wrapper in force.
        cmd.env("RUSTC_WRAPPER", "").env("RUSTC_WORKSPACE_WRAPPER", "");
        for f in skip_fns {
            cmd.args(["--skip", f]);
        }
        let (status, stdout, stderr) = world::run_capture(&mut cmd, TESTS_DEADLINE)?;
        let passed = stdout
            .lines()
            .filter_map(|l| l.strip_prefix("test ")?.strip_suffix(" ... ok"))
            .filter_map(|path| path.rsplit("::").next().map(String::from))
            .collect();
        Ok(TestRun { green: status.success(), passed, output: format!("{stdout}{stderr}") })
    }
}

/// `test-registry` as the world's own configuration names it, at the world's wall clock.
pub struct WorldRegistry<'a>(pub &'a Path);

impl WorldRegistry<'_> {
    fn said(&self, args: &[&str]) -> Result<String, String> {
        let shell = std::iter::once("test-registry".to_string()).chain(args.iter().map(|a| format!("'{a}'"))).collect::<Vec<_>>().join(" ");
        let out = crate::pilgrim::exec(self.0, &shell)?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).into_owned())
        } else {
            Err(format!("test-registry {}: {}{}", args.first().copied().unwrap_or(""), String::from_utf8_lossy(&out.stdout).trim(), String::from_utf8_lossy(&out.stderr).trim()))
        }
    }
}

impl RoundRegistry for WorldRegistry<'_> {
    fn deny_list(&self) -> Result<String, String> {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_err(|e| e.to_string())?.as_secs();
        self.said(&["deny-list", "--at", &now.to_string()])
    }
    fn activate(&self, test: &str, reason: &str) -> Result<(), String> {
        self.said(&["activate", "--test", test, "--reason", reason, "--actor", "batcher"]).map(|_| ())
    }
    fn clear_landed(&self, bead: &str) -> Result<Vec<String>, String> {
        Ok(self.said(&["clear-landed", "--bead", bead, "--actor", "queue"])?.lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from).collect())
    }
}

/// `sim world land <world> <bead>`: the report, and whether the round landed.
pub fn land_in_world(world: &Path, bead: &str) -> Result<(String, bool), String> {
    let out = land(&world.join("work"), world::LANDING_BASE, bead, &WorldRegistry(world), &CargoTests)?;
    Ok((out.report(), out.green))
}
