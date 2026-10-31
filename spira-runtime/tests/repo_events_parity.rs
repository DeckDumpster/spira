//! The repo-events handler says the lines repo-events.sh said, for run JSON shaped as the forge
//! serves it. The script hard-codes its own paths and cannot run in a fixture, so its case table
//! is the oracle, quoted here:
//!
//! ```text
//! [ "$st" = completed ] || continue
//! k="run|$slug|$id|$co"; seen "$k" && continue; mark "$k"
//! case "$wf" in
//!     Acceptance*) say 1 "$slug acceptance run $id ${co^^} ($ev) — $t" ;;
//!     *) [ "$br" = main ] || [ "$br" = master ] || continue
//!        case "$co" in success|skipped) say 0 "$slug $wf on $br $co — $t" ;;
//!                      *) say 1 "$slug $wf on $br ${co^^} (run $id) — $t" ;; esac ;;
//! esac
//! ```
//!
//! `say 0` prints to the watchd log only; `say 1` also appends to the Concierge inbox, so only
//! `say 1` is a line here. The PR half of the script is `pr-notify`'s now: the last test drives it
//! over fixture PRs to show nothing the script said about PRs is lost.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};

use runtime::host::EffectRunner;
use runtime::ports::VirtualClock;
use runtime::{Engine, EventKind, Executor, Journal, MemJournal, Outcome, Registration};
use serde_json::{json, Value};
use spira_runtime::forge::Forge;
use spira_runtime::watchers::pr_notify::PrNotify;
use spira_runtime::watchers::repo_events::RepoEvents;
use spira_runtime::world::World;
use spira_runtime::Effects;

const INTERVAL_MS: u64 = 45_000;

#[derive(Clone)]
struct Pr {
    n: u64,
    checks: Vec<(&'static str, &'static str, &'static str)>,
}

#[derive(Clone, Default)]
struct Repo {
    runs: Vec<Value>,
    open: Vec<Pr>,
    gone: Vec<(u64, &'static str)>,
    down: bool,
}

type Forges = std::collections::BTreeMap<&'static str, Repo>;

fn run(id: u64, wf: &str, status: &str, concl: Option<&str>, branch: &str, event: &str, title: &str) -> Value {
    json!({"id": id, "name": wf, "status": status, "conclusion": concl, "head_branch": branch, "event": event, "display_title": title})
}

fn route(forges: &Forges, path: &str) -> Option<Value> {
    let rest = path.split('?').next()?.strip_prefix("/repos/acme/")?;
    let (name, rest) = rest.split_once('/')?;
    let repo = forges.get(name).filter(|r| !r.down)?;
    if rest == "actions/runs" {
        return Some(json!({"total_count": repo.runs.len(), "workflow_runs": repo.runs}));
    }
    if rest == "pulls" {
        return Some(Value::Array(repo.open.iter().map(|p| json!({"number": p.n, "title": format!("pr {}", p.n), "head": {"ref": "feature/x", "sha": format!("sha{}", p.n)}})).collect()));
    }
    if let Some(n) = rest.strip_prefix("pulls/") {
        let n: u64 = n.parse().ok()?;
        let state = repo.gone.iter().find(|(m, _)| *m == n)?.1;
        return Some(json!({"state": if state == "OPEN" { "open" } else { "closed" }, "merged": state == "MERGED", "merge_commit_sha": null, "body": ""}));
    }
    let sha = rest.strip_prefix("commits/")?.strip_suffix("/check-runs")?;
    let p = repo.open.iter().find(|p| format!("sha{}", p.n) == sha)?;
    Some(json!({"check_runs": p.checks.iter().map(|(n, s, c)| json!({"name": n, "status": s, "conclusion": if c.is_empty() { Value::Null } else { json!(c) }})).collect::<Vec<_>>()}))
}

fn serve_forge(forges: Arc<Mutex<Forges>>) -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let api = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut line = String::new();
            let mut reader = BufReader::new(&stream);
            reader.read_line(&mut line).unwrap();
            let mut header = String::new();
            while reader.read_line(&mut header).is_ok_and(|n| n > 2) {
                header.clear();
            }
            let path = line.split_whitespace().nth(1).unwrap_or("").to_string();
            let (status, body) = match route(&forges.lock().unwrap(), &path) {
                Some(v) => ("200 OK", v.to_string()),
                None => ("503 Service Unavailable", "{}".to_string()),
            };
            let mut w = &stream;
            let _ = write!(w, "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
        }
    });
    api
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git").arg("-C").arg(dir).args(["-c", "user.email=t@t", "-c", "user.name=t"]).args(args).output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

struct Fixture {
    dir: testkit::TempDir,
    forges: Arc<Mutex<Forges>>,
    engine: Engine,
    journal: Arc<MemJournal>,
    clock: VirtualClock,
    fx: Effects,
    inbox_seen: usize,
    watcher: &'static str,
}

impl Fixture {
    fn build(name: &str, repos: &[&'static str], reg: Registration, boot: (&'static str, &'static str, EventKind), watcher: &'static str) -> Fixture {
        let dir = testkit::TempDir::new(&format!("rev-{name}"));
        let root = dir.path().to_path_buf();
        fs::create_dir_all(root.join("run")).unwrap();
        let mut rows = String::new();
        for repo in repos {
            let path = root.join(repo);
            fs::create_dir_all(&path).unwrap();
            git(&path, &["init", "-q"]);
            git(&path, &["remote", "add", "origin", &format!("file:///nonexistent/acme/{repo}.git")]);
            rows.push_str(&format!("{repo} | {} | queue.forge | origin/main | |\n", path.display()));
        }
        fs::write(root.join("rows"), rows).unwrap();
        let forges: Arc<Mutex<Forges>> = Arc::new(Mutex::new(repos.iter().map(|r| (*r, Repo::default())).collect()));
        let api = serve_forge(forges.clone());
        let fx = Effects {
            world: World { run_dir: root.join("run"), queue_dir: root.join("run/queue"), repo_map: root.join("rows"), home_repo: "home".into(), inbox: root.join("inbox"), lc_socket: root.join("no.sock"), ..World::default() },
            forge: Forge { api, token: String::new() },
            ..Effects::default()
        };
        let clock = VirtualClock::new(1_800_000_000_000);
        let journal = MemJournal::new();
        let mut engine = Engine::open(journal.clone(), Arc::new(clock.clone()), vec![reg], 0).unwrap();
        let (id, stream, kind) = boot;
        engine.submit(id, stream, kind).unwrap();
        Fixture { dir, forges, engine, journal, clock, fx, inbox_seen: 0, watcher }
    }

    fn repo_events(repos: &[&'static str]) -> Fixture {
        Fixture::build("events", repos, RepoEvents { interval_ms: INTERVAL_MS }.registration(), RepoEvents::boot(), "repo-events")
    }

    /// One tick: let the watcher look and return the inbox lines it appended.
    fn tick(&mut self) -> Vec<String> {
        self.engine.run_until_idle().unwrap();
        loop {
            let pending = self.engine.pending_effects();
            if pending.is_empty() {
                break;
            }
            for p in pending {
                let out = if p.spec.name == spira_runtime::watchers::pr_notify::OPERATOR_NOTE { Ok("noted".to_string()) } else { self.fx.run(&p.key, &p.spec) };
                self.engine.complete(&p.key, out.map_or_else(Outcome::Failed, Outcome::Succeeded)).unwrap();
            }
            self.engine.run_until_idle().unwrap();
        }
        self.clock.advance(INTERVAL_MS);
        let inbox = fs::read_to_string(self.dir.path().join("inbox")).unwrap_or_default();
        let tag = format!(" [watch:{}] ", self.watcher);
        let lines = inbox.lines().skip(self.inbox_seen).map(|l| l.split_once(&tag).expect("an inbox row names its watcher").1.to_string()).collect();
        self.inbox_seen = inbox.lines().count();
        lines
    }

    fn set(&self, repo: &str, f: impl FnOnce(&mut Repo)) {
        f(self.forges.lock().unwrap().get_mut(repo).unwrap());
    }

    fn events(&self) -> usize {
        self.journal.read_from(0, 100_000).unwrap().len()
    }
}

/// A fixture already past its silent first adoption of `existing`.
fn adopted(existing: Vec<Value>) -> Fixture {
    let mut f = Fixture::repo_events(&["widgets"]);
    f.set("widgets", |r| r.runs = existing);
    assert!(f.tick().is_empty(), "the first start adopts the forge silently");
    f
}

#[test]
fn an_acceptance_run_is_said_whatever_its_result() {
    let mut f = adopted(vec![run(1, "CI", "completed", Some("success"), "main", "push", "old")]);
    f.set("widgets", |r| r.runs.insert(0, run(41099299609, "Acceptance (daily)", "completed", Some("failure"), "main", "workflow_dispatch", "Acceptance (daily)")));
    assert_eq!(f.tick(), ["acme/widgets acceptance run 41099299609 FAILURE (workflow_dispatch) — Acceptance (daily)"]);
    f.set("widgets", |r| r.runs.insert(0, run(41099299610, "Acceptance (nightly)", "completed", Some("success"), "feature/x", "schedule", "nightly")));
    assert_eq!(f.tick(), ["acme/widgets acceptance run 41099299610 SUCCESS (schedule) — nightly"]);
}

#[test]
fn a_main_run_is_said_unless_it_succeeded_or_was_skipped() {
    let mut f = adopted(vec![]);
    f.set("widgets", |r| {
        r.runs = vec![
            run(5, "CI", "completed", Some("success"), "main", "push", "green"),
            run(4, "Deploy", "completed", Some("skipped"), "master", "push", "skipped"),
            run(3, "Deploy", "completed", Some("cancelled"), "master", "push", "cancelled"),
            run(2, "CI", "completed", Some("failure"), "main", "push", "broke"),
        ]
    });
    assert_eq!(f.tick(), ["acme/widgets Deploy on master CANCELLED (run 3) — cancelled", "acme/widgets CI on main FAILURE (run 2) — broke"]);
}

#[test]
fn a_feature_branch_failure_and_an_unfinished_run_stay_silent() {
    let mut f = adopted(vec![]);
    f.set("widgets", |r| {
        r.runs = vec![run(7, "CI", "completed", Some("failure"), "feature/x", "pull_request", "red pr"), run(8, "CI", "in_progress", None, "main", "push", "running"), run(9, "CI", "queued", None, "main", "push", "queued")]
    });
    assert!(f.tick().is_empty());
}

#[test]
fn a_run_seen_twice_is_said_once_and_an_unfinished_run_is_said_when_it_finishes() {
    let mut f = adopted(vec![]);
    f.set("widgets", |r| r.runs = vec![run(2, "CI", "in_progress", None, "main", "push", "slow")]);
    assert!(f.tick().is_empty());
    f.set("widgets", |r| r.runs = vec![run(2, "CI", "completed", Some("failure"), "main", "push", "slow")]);
    assert_eq!(f.tick(), ["acme/widgets CI on main FAILURE (run 2) — slow"]);
    assert!(f.tick().is_empty(), "the same run is not said again");
}

#[test]
fn a_title_is_cut_and_escaped_as_the_script_printed_it() {
    let mut f = adopted(vec![]);
    let long = format!("a\tb{}", "x".repeat(100));
    f.set("widgets", |r| r.runs = vec![run(3, "CI", "completed", Some("failure"), "main", "push", &long)]);
    let said = f.tick();
    assert_eq!(said.len(), 1);
    assert!(said[0].ends_with(&format!(" — a\\tb{}", "x".repeat(77))), "{said:?}");
}

#[test]
fn the_script_state_file_seeds_the_carry_so_the_cutover_neither_repeats_nor_misses_a_run() {
    let mut f = Fixture::repo_events(&["widgets"]);
    fs::create_dir_all(f.dir.path().join("run/watchd")).unwrap();
    fs::write(f.dir.path().join("run/watchd/repo-events.state"), "pr|acme/widgets|1|OPEN|none\nrun|acme/widgets|10|failure\n").unwrap();
    f.set("widgets", |r| {
        r.runs = vec![run(11, "CI", "completed", Some("failure"), "main", "push", "missed while down"), run(10, "CI", "completed", Some("failure"), "main", "push", "already said")]
    });
    assert_eq!(f.tick(), ["acme/widgets CI on main FAILURE (run 11) — missed while down"]);
    assert!(f.tick().is_empty());
}

#[test]
fn ten_ticks_of_an_unchanged_forge_journal_nothing() {
    let mut f = adopted(vec![run(1, "CI", "completed", Some("failure"), "main", "push", "old")]);
    let after_first = f.events();
    assert!(after_first > 0, "the first tick journaled its state");
    for _ in 0..10 {
        assert!(f.tick().is_empty());
    }
    assert_eq!(f.events(), after_first, "an idle tick touches no journal");
}

#[test]
fn a_forge_failure_says_its_http_error_once() {
    let mut f = adopted(vec![]);
    f.set("widgets", |r| r.down = true);
    let said = f.tick();
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(said[0].starts_with("REPO-EVENTS FAILED: acme/widgets: ") && said[0].contains("status code 503"), "{said:?}");
    assert!(f.tick().is_empty() && f.tick().is_empty(), "a standing failure is not said again");
    f.set("widgets", |r| {
        r.down = false;
        r.runs = vec![run(1, "CI", "completed", Some("failure"), "main", "push", "after")];
    });
    assert_eq!(f.tick(), ["acme/widgets CI on main FAILURE (run 1) — after"], "the forge back is judged against what was carried");
}

#[test]
fn every_repository_in_the_map_is_watched() {
    let mut f = Fixture::repo_events(&["widgets", "gadgets"]);
    assert!(f.tick().is_empty());
    for repo in ["widgets", "gadgets"] {
        f.set(repo, |r| r.runs = vec![run(1, "CI", "completed", Some("failure"), "main", "push", "x")]);
    }
    let said = f.tick();
    assert_eq!(said.len(), 2, "{said:?}");
}

#[test]
fn pr_notify_still_says_every_pr_transition_the_script_said() {
    let (reg, boot) = (PrNotify { interval_ms: INTERVAL_MS, deploy_giveup_secs: 777 }.registration(), PrNotify::boot());
    let mut f = Fixture::build("prs", &["widgets"], reg, boot, "pr-notify");
    let pr = |n, checks: &[(&'static str, &'static str, &'static str)]| Pr { n, checks: checks.to_vec() };
    let (running, red, green) = (("suites", "in_progress", ""), ("suites", "completed", "failure"), ("suites", "completed", "success"));
    f.set("widgets", |r| r.open = vec![pr(1, &[running]), pr(2, &[running])]);
    assert_eq!(f.tick(), ["OPENED #1 pr 1 [widgets]", "OPENED #2 pr 2 [widgets]"]);
    f.set("widgets", |r| r.open = vec![pr(1, &[red]), pr(2, &[red])]);
    assert_eq!(f.tick(), ["FAIL RED #1 pr 1 [widgets]: suites", "FAIL RED #2 pr 2 [widgets]: suites"]);
    f.set("widgets", |r| r.open = vec![pr(1, &[green]), pr(2, &[green])]);
    assert_eq!(f.tick(), ["GREEN #1 pr 1 [widgets]: lands automatically", "GREEN #2 pr 2 [widgets]: lands automatically"]);
    f.set("widgets", |r| {
        r.open.clear();
        r.gone = vec![(1, "MERGED"), (2, "CLOSED")];
    });
    assert_eq!(f.tick(), ["MERGED #1 pr 1 [widgets]", "CLOSED #2 pr 2 [widgets]"]);
}
