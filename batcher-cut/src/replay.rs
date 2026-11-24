//! `batcher save-round`: a kept round written as a replay fixture that carries no secret, host,
//! user or path. Inputs are the round's results folder, its lifecycle row and git; the lifecycle
//! answer and the git answers are handed in, so the fixture is built without a spawned process.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::vm::{LINT_FINDINGS_FILE, ROUND_COMMITS_FILE};

const FIXED_FILES: &[&str] = &["deny-list", ROUND_COMMITS_FILE, "runner.place", "run.events", "unit.json", "integration.json", "workspace-build.out", LINT_FINDINGS_FILE];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Member {
    pub id: String,
    pub tip: String,
    pub changed: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoundFixture {
    pub batch: String,
    pub base: String,
    pub cut_head: String,
    pub head: String,
    pub members: Vec<Member>,
    pub red_suites: Vec<String>,
    pub place: Option<String>,
    pub round_commit_paths: Vec<String>,
    pub tree_suites: Vec<String>,
}

/// What git answers about the round's repository.
pub trait Git {
    fn has_commit(&self, sha: &str) -> bool;
    fn changed_paths(&self, base: &str, tip: &str) -> Result<Vec<String>, String>;
    fn commit_paths(&self, shas: &[String]) -> Vec<String>;
    fn suites(&self, head: &str) -> Result<Vec<String>, String>;
}

/// What names this host, this user and this run; each is replaced by a placeholder.
#[derive(Debug, Clone, Default)]
pub struct Names {
    pub run: String,
    pub home: String,
    pub host: String,
    pub user: String,
}

/// A fixture ready to write: every file's scrubbed text, nothing yet on disk.
#[derive(Debug)]
pub struct Built {
    pub round: RoundFixture,
    pub files: Vec<(String, String)>,
}

fn re(p: &str) -> Regex {
    Regex::new(p).expect("static regex")
}

pub fn scrub_line(line: &str, n: &Names) -> String {
    let mut s = forge::cmds::redact_line(line);
    for (needle, to) in [(&n.run, "<run>"), (&n.home, "<home>")] {
        if !needle.is_empty() {
            s = s.replace(needle.as_str(), to);
        }
    }
    s = re(r"[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}").replace_all(&s, "<mail>").into_owned();
    s = re(r"\b[0-9]{1,3}(\.[0-9]{1,3}){3}\b").replace_all(&s, "<ip>").into_owned();
    s = re(r"[A-Za-z0-9_.@:-]+\.(service|timer|socket|target|path|mount|slice|scope)\b").replace_all(&s, "<unit>").into_owned();
    for (needle, to) in [(&n.host, "<host>"), (&n.user, "<user>")] {
        if !needle.is_empty() {
            s = s.replace(needle.as_str(), to);
        }
    }
    s
}

fn scrub_text(file: &str, text: &str, n: &Names, surviving: &regex::bytes::Regex) -> Result<String, String> {
    let mut out = String::with_capacity(text.len());
    for (i, line) in text.lines().enumerate() {
        let clean = scrub_line(line, n);
        if surviving.is_match(clean.as_bytes()) {
            return Err(format!("{file}:{}: a pattern the inventory fence refuses survives scrubbing", i + 1));
        }
        out.push_str(&clean);
        out.push('\n');
    }
    Ok(out)
}

fn field(v: &serde_json::Value, k: &str) -> Result<String, String> {
    v[k].as_str().filter(|s| !s.is_empty()).map(str::to_string).ok_or_else(|| format!("the lifecycle row has no {k}"))
}

fn red_suites_of(passes: &[serde_json::Value]) -> Vec<String> {
    passes
        .iter()
        .rev()
        .find(|p| p["verdict"] == "red")
        .and_then(|p| p["red_suites"].as_array())
        .map(|a| a.iter().filter_map(|s| s.as_str().map(str::to_string)).collect())
        .unwrap_or_default()
}

fn place_of(text: &str) -> Option<String> {
    text.lines().find_map(|l| l.strip_prefix("place=")).map(|v| v.trim().to_string()).filter(|p| !p.is_empty() && p != "unrecorded")
}

/// `batches`: the answer to `spira-lc list --batches --batch <batch>`.
pub fn build(batches: &str, batch: &str, results: &Path, git: &dyn Git, names: &Names) -> Result<Built, String> {
    if !results.is_dir() {
        return Err(format!("{}: no results folder for round {batch}", results.display()));
    }
    let rows: Vec<serde_json::Value> = serde_json::from_str(batches).map_err(|e| format!("the lifecycle answer is not a batch list: {e}"))?;
    let row = rows.iter().find(|b| b["batch_id"] == batch).ok_or_else(|| format!("{batch}: the lifecycle lists no such batch"))?;
    let base = field(row, "base")?;
    let head = field(row, "head")?;
    let passes = row["passes"].as_array().cloned().unwrap_or_default();
    let cut_head = passes.first().and_then(|p| p["head"].as_str()).map(str::to_string).unwrap_or_else(|| head.clone());
    if !git.has_commit(&base) {
        return Err(format!("base {base} is unreadable in the repository"));
    }
    let mut members = vec![];
    for m in row["members"].as_array().into_iter().flatten() {
        let (id, tip) = (field(m, "bead_id")?, field(m, "tip")?);
        if !git.has_commit(&tip) {
            return Err(format!("tip {tip} of {id} is unreadable in the repository"));
        }
        let changed = git.changed_paths(&base, &tip)?;
        members.push(Member { id, tip, changed });
    }
    let deny = results.join("deny-list");
    if !deny.is_file() {
        return Err(format!("{}: the round kept no deny list", deny.display()));
    }
    let commits: Vec<String> = fs::read_to_string(results.join(ROUND_COMMITS_FILE))
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect();
    let place = place_of(&fs::read_to_string(results.join("runner.place")).unwrap_or_default());
    let red_suites = red_suites_of(&passes);

    let mut names_to_copy: BTreeSet<String> = FIXED_FILES.iter().map(|s| s.to_string()).collect();
    for e in fs::read_dir(results).map_err(|e| format!("{}: {e}", results.display()))?.flatten() {
        let f = e.file_name().to_string_lossy().into_owned();
        if f.ends_with(".result") {
            names_to_copy.insert(f);
        }
    }
    for red in &red_suites {
        names_to_copy.insert(format!("{red}.out"));
        names_to_copy.insert(format!("{red}.tap.json"));
    }
    let surviving = spira_lint::rules::inventory::pattern_re(&[])?;
    let mut files = vec![];
    for f in names_to_copy {
        let Ok(text) = fs::read_to_string(results.join(&f)) else { continue };
        files.push((format!("results/{f}"), scrub_text(&f, &text, names, &surviving)?));
    }
    let round = RoundFixture {
        batch: batch.to_string(),
        base,
        cut_head,
        head: head.clone(),
        members,
        red_suites,
        place,
        round_commit_paths: git.commit_paths(&commits),
        tree_suites: git.suites(&head)?,
    };
    let json = serde_json::to_string_pretty(&round).map_err(|e| e.to_string())?;
    files.push(("round.json".into(), scrub_text("round.json", &json, names, &surviving)?));
    Ok(Built { round, files })
}

/// Write `built` under `dir`, which must be absent or empty; a refusal leaves no `dir`.
pub fn write(built: &Built, dir: &Path) -> Result<(), String> {
    if dir.is_dir() && fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?.next().is_some() {
        return Err(format!("{}: not empty", dir.display()));
    }
    let put = || -> std::io::Result<()> {
        for (name, text) in &built.files {
            let p: PathBuf = dir.join(name);
            fs::create_dir_all(p.parent().unwrap_or(dir))?;
            fs::write(p, text)?;
        }
        Ok(())
    };
    put().map_err(|e| {
        let _ = fs::remove_dir_all(dir);
        format!("{}: {e}", dir.display())
    })
}

pub struct RepoGit<'a>(pub &'a crate::io::Repo);

impl Git for RepoGit<'_> {
    fn has_commit(&self, sha: &str) -> bool {
        spira_config::bounded::bounded("git")
            .arg("-C")
            .arg(&self.0.path)
            .args(["cat-file", "-e", &format!("{sha}^{{commit}}")])
            .output()
            .is_ok_and(|o| o.status.success())
    }
    fn changed_paths(&self, base: &str, tip: &str) -> Result<Vec<String>, String> {
        crate::io::changed_paths_checked(self.0, base, tip)
    }
    fn commit_paths(&self, shas: &[String]) -> Vec<String> {
        crate::vm::own_commit_paths(&self.0.path, shas)
    }
    fn suites(&self, head: &str) -> Result<Vec<String>, String> {
        crate::io::all_suites(self.0, head)
    }
}

pub const USAGE: &str = "batcher save-round --repo R --batch ID --out DIR [--run DIR] [--db DIR] [--home DIR]\n  writes the kept round as a replay fixture (DIR/round.json, DIR/results/) with credentials, this host's name, user, addresses and paths replaced by placeholders\n  exit: 0 written; 1 refused (no results folder, batch not listed, unreadable base or tip, no deny list, DIR not empty, a pattern the inventory fence refuses survives: file:line named, DIR removed); 2 usage";

pub fn names_here(run: &Path, home: &Path) -> Names {
    Names {
        run: run.to_string_lossy().into_owned(),
        home: std::env::var("HOME").unwrap_or_else(|_| home.to_string_lossy().into_owned()),
        host: std::fs::read_to_string("/proc/sys/kernel/hostname").map(|s| s.trim().to_string()).unwrap_or_default(),
        user: std::env::var("USER").unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeGit {
        unreadable: Vec<&'static str>,
    }
    impl Git for FakeGit {
        fn has_commit(&self, sha: &str) -> bool {
            !self.unreadable.contains(&sha)
        }
        fn changed_paths(&self, _: &str, tip: &str) -> Result<Vec<String>, String> {
            Ok(vec![format!("src/{tip}.rs")])
        }
        fn commit_paths(&self, shas: &[String]) -> Vec<String> {
            shas.iter().map(|s| format!("spira/{s}.sh")).collect()
        }
        fn suites(&self, _: &str) -> Result<Vec<String>, String> {
            Ok(vec!["spira/test-a.sh".into()])
        }
    }

    const BATCHES: &str = r#"[{"batch_id":"b1","repo":"r","base":"base1","head":"head2","members":[{"batch_id":"b1","bead_id":"sp-a","tip":"tipa"},{"batch_id":"b1","bead_id":"sp-b","tip":"tipb"}],
        "passes":[{"n":1,"head":"head1","verdict":"red","red_suites":["test-a.sh"]},{"n":2,"head":"head2","verdict":"green"}]}]"#;

    fn names() -> Names {
        Names { run: "/srv/spira-run".into(), home: "/home/zed".into(), host: "box-17".into(), user: "zed".into() }
    }

    fn results() -> testkit::TempDir {
        let d = testkit::TempDir::new("replay");
        let w = |f: &str, t: &str| fs::write(d.path().join(f), t).unwrap();
        w("deny-list", "# test-registry deny-list at=1 rows=0\n");
        w("round-commits", "c1\n");
        w("runner.place", "place=ec2\n");
        w("test-a.sh.result", "red 1 2\n");
        w("test-a.sh.out", "token=abcd1234 at /home/zed/x on box-17 10.1.2.3 mail a@corp.io unit spira-q.service\nran under /srv/spira-run/rounds\n");
        w("test-a.sh.tap.json", "{}\n");
        w("unit-timings", "not needed\n");
        d
    }

    fn git() -> FakeGit {
        FakeGit { unreadable: vec![] }
    }

    #[test_id(ts5tv6n)]
    #[test]
    fn a_folder_holding_a_token_home_path_address_and_host_name_yields_none_of_them_and_only_the_listed_files() {
        let d = results();
        let built = build(BATCHES, "b1", d.path(), &git(), &names()).unwrap();
        let out = &built.files.iter().find(|(f, _)| f == "results/test-a.sh.out").unwrap().1;
        for leak in ["abcd1234", "/home/zed", "box-17", "10.1.2.3", "corp.io", "spira-q.service", "/srv/spira-run"] {
            assert!(!out.contains(leak), "{leak} survived in {out}");
        }
        for kept in ["<home>", "<host>", "<ip>", "<mail>", "<unit>", "<run>"] {
            assert!(out.contains(kept), "{kept} missing in {out}");
        }
        let listed: Vec<&str> = built.files.iter().map(|(f, _)| f.as_str()).collect();
        assert!(!listed.contains(&"results/unit-timings"));
        for f in ["results/deny-list", "results/round-commits", "results/runner.place", "results/test-a.sh.result", "results/test-a.sh.tap.json", "round.json"] {
            assert!(listed.contains(&f), "{f} not written: {listed:?}");
        }
        assert_eq!(built.round.cut_head, "head1");
        assert_eq!(built.round.head, "head2");
        assert_eq!(built.round.red_suites, ["test-a.sh"]);
        assert_eq!(built.round.place.as_deref(), Some("ec2"));
        assert_eq!(built.round.round_commit_paths, ["spira/c1.sh"]);
        assert_eq!(built.round.members[1].changed, ["src/tipb.rs"]);
    }

    #[test_id(tvnk53f)]
    #[test]
    fn a_surviving_pattern_refuses_naming_file_and_line_and_leaves_no_out_dir() {
        let d = results();
        fs::write(d.path().join("test-a.sh.out"), "fine\n/Users/someone/x\n").unwrap();
        let err = build(BATCHES, "b1", d.path(), &git(), &names()).unwrap_err();
        assert!(err.starts_with("test-a.sh.out:2:"), "{err}");

        let ok = results();
        let built = build(BATCHES, "b1", ok.path(), &git(), &names()).unwrap();
        let out = testkit::TempDir::new("replay");
        let dir = out.path().join("fixture");
        write(&built, &dir).unwrap();
        assert!(dir.join("round.json").is_file());
        let mut bad = built;
        bad.files.push(("x".into(), "y".into()));
        assert!(write(&bad, &dir).unwrap_err().contains("not empty"));
    }

    #[test_id(t4mfobs)]
    #[test]
    fn a_missing_deny_list_folder_batch_base_or_tip_refuses() {
        let d = results();
        fs::remove_file(d.path().join("deny-list")).unwrap();
        assert!(build(BATCHES, "b1", d.path(), &git(), &names()).unwrap_err().contains("deny list"));
        assert!(build(BATCHES, "b1", &d.path().join("nope"), &git(), &names()).unwrap_err().contains("no results folder"));
        let d = results();
        assert!(build(BATCHES, "other", d.path(), &git(), &names()).unwrap_err().contains("no such batch"));
        assert!(build(BATCHES, "b1", d.path(), &FakeGit { unreadable: vec!["base1"] }, &names()).unwrap_err().contains("base"));
        assert!(build(BATCHES, "b1", d.path(), &FakeGit { unreadable: vec!["tipb"] }, &names()).unwrap_err().contains("tipb"));
    }

    #[test_id(tzvt4gn)]
    #[test]
    fn round_json_round_trips() {
        let d = results();
        let built = build(BATCHES, "b1", d.path(), &git(), &names()).unwrap();
        let json = &built.files.iter().find(|(f, _)| f == "round.json").unwrap().1;
        let back: RoundFixture = serde_json::from_str(json).unwrap();
        assert_eq!(back, built.round);
    }
}
