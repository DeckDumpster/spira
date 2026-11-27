//! The history bound against a real Dolt database, opened with no server.

#[macro_use]
extern crate spira_test_id;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use store_persist::history::{self, Sql};
use store_persist::tree_bytes;
use testkit::TempDir;

const KEEP: u64 = 6;
const COMMITS: u64 = 120;

fn dolt_on_path() -> bool {
    Command::new("dolt").arg("version").stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok()
}

macro_rules! need_dolt {
    () => {
        if !dolt_on_path() {
            eprintln!("SKIP: no dolt binary on PATH");
            return;
        }
    };
}

struct Repo {
    dir: PathBuf,
    home: PathBuf,
}

impl Sql for Repo {
    fn run(&self, script: &str, csv: bool) -> Result<String, String> {
        let mut cmd = Command::new("dolt");
        cmd.current_dir(&self.dir).env("HOME", &self.home).env("DOLT_ROOT_PATH", &self.home).arg("sql");
        if csv {
            cmd.args(["-r", "csv"]);
        }
        let mut child = cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().expect("dolt must be on PATH for this test");
        // Fed from a thread: dolt answers as it reads, so a script and its output that each
        // outgrow a pipe deadlock if the script is written before the output is drained.
        let (mut stdin, script) = (child.stdin.take().unwrap(), script.to_owned());
        let feed = std::thread::spawn(move || std::io::Write::write_all(&mut stdin, script.as_bytes()));
        let out = child.wait_with_output().unwrap();
        feed.join().unwrap().unwrap();
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).into_owned())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).into_owned())
        }
    }
}

impl Repo {
    fn tool(&self, args: &[&str]) {
        let st = Command::new("dolt").current_dir(&self.dir).env("HOME", &self.home).env("DOLT_ROOT_PATH", &self.home).args(args).output().unwrap();
        assert!(st.status.success(), "dolt {args:?}: {}", String::from_utf8_lossy(&st.stderr));
    }
    fn noms(&self) -> u64 {
        tree_bytes(&self.dir.join(".dolt/noms"))
    }
    fn rows(&self) -> String {
        self.run("SELECT id, body FROM t ORDER BY id;\n", true).unwrap()
    }
}

fn fixture(tmp: &Path, name: &str) -> Repo {
    fixture_of(tmp, name, COMMITS)
}

fn fixture_of(tmp: &Path, name: &str, commits: u64) -> Repo {
    let (dir, home) = (tmp.join(name), tmp.join(format!("{name}-home")));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    let r = Repo { dir, home };
    r.tool(&["config", "--global", "--add", "user.name", "t"]);
    r.tool(&["config", "--global", "--add", "user.email", "t@example.invalid"]);
    r.tool(&["init", "-b", "main"]);
    let mut script = String::from("CREATE TABLE t (id INT PRIMARY KEY, body VARCHAR(2000));\nCALL DOLT_COMMIT('-Am', 'schema');\n");
    for i in 0..commits {
        script.push_str(&format!(
            "INSERT INTO t VALUES ({i}, CONCAT('{i}-', REPEAT('x', 1500))) ON DUPLICATE KEY UPDATE body = CONCAT('{i}-', REPEAT('y', 1500));\nCALL DOLT_COMMIT('-Am', 'bd: update {i}');\n"
        ));
    }
    script.push_str("INSERT INTO t VALUES (9999, 'uncommitted');\n");
    r.run(&script, false).unwrap();
    r
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let p = e.unwrap().path();
        let dst = to.join(p.file_name().unwrap());
        if p.is_dir() {
            copy_tree(&p, &dst);
        } else {
            std::fs::copy(&p, &dst).unwrap();
        }
    }
}

fn squash_and_collect(r: &Repo, keep: u64) {
    let tip = history::squash(r, keep).unwrap();
    history::graft(r, &tip).unwrap();
    r.run("CALL DOLT_GC('--full');\n", false).unwrap();
}

#[test_id(tevy2v3)]
#[test]
fn a_squash_leaves_keep_plus_one_commits_the_same_data_and_the_uncommitted_write() {
    need_dolt!();
    let tmp = TempDir::new("store-persist-history");
    let r = fixture_of(tmp.path(), "db", KEEP * 4);
    assert_eq!(history::len(&r).unwrap(), KEEP * 4 + 2);
    let (rows, dirty) = (r.rows(), r.run("SELECT COUNT(*) AS n FROM dolt_diff_summary('HEAD', 'WORKING');\n", true).unwrap());
    squash_and_collect(&r, KEEP);
    assert_eq!(history::len(&r).unwrap(), history::length_after(KEEP));
    assert_eq!(r.rows(), rows, "the data at HEAD is byte-identical");
    assert_eq!(r.run("SELECT COUNT(*) AS n FROM dolt_diff_summary('HEAD', 'WORKING');\n", true).unwrap(), dirty, "the working set is untouched");
    let branches = r.run("SELECT name FROM dolt_branches;\n", true).unwrap();
    assert_eq!(branches.trim(), "name\nmain", "no side branch is left");
}

#[test_id(t1d7q02)]
#[test]
fn a_squash_catches_up_commits_made_while_the_side_branch_was_built() {
    need_dolt!();
    let tmp = TempDir::new("store-persist-history-late");
    let r = fixture(tmp.path(), "db");
    r.run("CALL DOLT_COMMIT('-Am', 'before');\n", false).unwrap();
    let tip = history::squash(&r, KEEP).unwrap();
    r.run("INSERT INTO t VALUES (5000, 'late one'); CALL DOLT_COMMIT('-Am', 'late 1');\nINSERT INTO t VALUES (5001, 'late two'); CALL DOLT_COMMIT('-Am', 'late 2');\n", false).unwrap();
    let rows = r.rows();
    history::graft(&r, &tip).unwrap();
    assert_eq!(r.rows(), rows);
    assert_eq!(history::len(&r).unwrap(), history::length_after(KEEP) + 2, "the two late commits follow the bounded history");
}

#[test_id(td3pxzy)]
#[test]
fn an_interrupted_rewrite_is_cleared_before_the_next_one_starts() {
    need_dolt!();
    let tmp = TempDir::new("store-persist-history-stale");
    let r = fixture_of(tmp.path(), "db", KEEP * 4);
    let tip = history::squash(&r, KEEP).unwrap();
    assert!(!tip.is_empty());
    assert!(r.run("SELECT name FROM dolt_branches;\n", true).unwrap().contains("store_persist_squash"));
    history::abandon(&r).unwrap();
    assert_eq!(r.run("SELECT name FROM dolt_branches;\n", true).unwrap().trim(), "name\nmain");
    history::squash(&r, KEEP).unwrap();
}

#[test_id(tb0v44o)]
#[test]
fn a_script_and_its_output_both_larger_than_a_pipe_do_not_deadlock() {
    need_dolt!();
    let tmp = TempDir::new("store-persist-history-pipe");
    let r = fixture_of(tmp.path(), "db", 1);
    let script = "SELECT id, body FROM t;\n".repeat(8000);
    let out = r.run(&script, true).unwrap();
    assert!(out.len() > 1 << 20, "output of {} B", out.len());
}

#[test_id(tb3qkql)]
#[test]
fn the_store_after_a_squash_fits_the_depth_curve_within_twice() {
    need_dolt!();
    let tmp = TempDir::new("store-persist-history-size");
    let full = fixture(tmp.path(), "db");
    let floor = fixture(tmp.path(), "floor");
    copy_tree(&full.dir, &tmp.path().join("bounded"));
    let bounded = Repo { dir: tmp.path().join("bounded"), home: full.home.clone() };
    squash_and_collect(&floor, 2);
    full.run("CALL DOLT_GC('--full');\n", false).unwrap();
    squash_and_collect(&bounded, KEEP);
    let (all, base) = (full.noms() as f64, floor.noms() as f64);
    let slope = (all - base) / COMMITS as f64;
    let predicted = base + slope * KEEP as f64;
    assert!(
        (bounded.noms() as f64) <= 2.0 * predicted,
        "bounded {} B vs depth-curve prediction {predicted:.0} B (floor {base:.0} B, full {all:.0} B over {COMMITS} commits)",
        bounded.noms()
    );
    assert!(bounded.noms() < full.noms(), "squashing shrank the store");
}

struct Counting<'a> {
    repo: &'a Repo,
    picks: std::cell::Cell<usize>,
    rebases: std::cell::Cell<usize>,
}

impl Sql for Counting<'_> {
    fn run(&self, script: &str, csv: bool) -> Result<String, String> {
        self.picks.set(self.picks.get() + script.matches("DOLT_CHERRY_PICK").count());
        self.rebases.set(self.rebases.get() + script.matches("DOLT_REBASE").count());
        self.repo.run(script, csv)
    }
}

/// `n` single-row commits after the schema commit, with the primary key of `t` dropped and
/// re-added in commit number `pk_at`; one uncommitted row follows.
fn pk_fixture(tmp: &Path, name: &str, n: u64, pk_at: u64) -> Repo {
    let (dir, home) = (tmp.join(name), tmp.join(format!("{name}-home")));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    let r = Repo { dir, home };
    r.tool(&["config", "--global", "--add", "user.name", "t"]);
    r.tool(&["config", "--global", "--add", "user.email", "t@example.invalid"]);
    r.tool(&["init", "-b", "main"]);
    let mut script = String::from("CREATE TABLE t (id INT PRIMARY KEY, body VARCHAR(200));\nCALL DOLT_COMMIT('-Am', 'schema');\n");
    for i in 0..n {
        if i == pk_at {
            script.push_str("ALTER TABLE t DROP PRIMARY KEY;\nALTER TABLE t ADD PRIMARY KEY (id, body);\n");
        }
        script.push_str(&format!("INSERT INTO t VALUES ({i}, 'row {i}');\nCALL DOLT_COMMIT('-Am', 'bd: update {i}');\n"));
    }
    script.push_str("INSERT INTO t VALUES (9999, 'uncommitted');\n");
    r.run(&script, false).unwrap();
    r
}

fn assert_squashed_intact(r: &Repo, exact: bool) -> usize {
    let (rows, schema) = (r.rows(), r.run("SHOW CREATE TABLE t;\n", true).unwrap());
    let c = Counting { repo: r, picks: 0.into(), rebases: 0.into() };
    let tip = history::squash(&c, KEEP).unwrap();
    history::graft(&c, &tip).unwrap();
    let len = history::len(r).unwrap();
    if exact {
        assert_eq!(len, history::length_after(KEEP));
    } else {
        assert!(len <= history::length_after(KEEP), "log of {len}");
    }
    assert_eq!(r.rows(), rows, "rows at HEAD");
    assert_eq!(r.run("SHOW CREATE TABLE t;\n", true).unwrap(), schema, "schema at HEAD");
    assert_eq!(r.run("SELECT COUNT(*) AS n FROM t WHERE id = 9999;\n", true).unwrap().trim(), "n\n1", "the uncommitted row survives");
    assert_eq!(r.run("SELECT name FROM dolt_branches;\n", true).unwrap().trim(), "name\nmain", "no side branch is left");
    assert_eq!(c.rebases.get(), 0, "no rebase replays history");
    c.picks.get()
}

#[test_id(tml9vjj)]
#[test]
fn a_primary_key_change_among_the_squashed_commits_does_not_stop_the_squash() {
    need_dolt!();
    let tmp = TempDir::new("store-persist-history-pk-old");
    let r = pk_fixture(tmp.path(), "db", 30, 5);
    assert_squashed_intact(&r, true);
}

#[test_id(t2cok1e)]
#[test]
fn a_primary_key_change_among_the_kept_commits_does_not_stop_the_squash() {
    need_dolt!();
    let tmp = TempDir::new("store-persist-history-pk-kept");
    let r = pk_fixture(tmp.path(), "db", 30, 28);
    assert_squashed_intact(&r, false);
}

#[test_id(tphzlfc)]
#[test]
fn the_cherry_picks_and_the_log_length_do_not_grow_with_the_history() {
    need_dolt!();
    let tmp = TempDir::new("store-persist-history-bounded");
    let small = pk_fixture(tmp.path(), "small", 20, 3);
    let large = pk_fixture(tmp.path(), "large", 80, 3);
    let (a, b) = (assert_squashed_intact(&small, true), assert_squashed_intact(&large, true));
    assert_eq!(a, b);
    assert!((a as u64) < KEEP);
}
